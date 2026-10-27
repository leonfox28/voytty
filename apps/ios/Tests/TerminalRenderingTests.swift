import XCTest
import UIKit
@testable import Zterm

@MainActor
final class TerminalRenderingTests: XCTestCase {
    private func cell(_ text: String = " ", width: UInt8 = 1, attributes: UInt8 = 0, underline: UInt8 = 0) -> NativeCell {
        NativeCell(text: text, width: width, foreground: 0xffffffff, background: 0xff000000,
                   attributes: attributes, underline: underline, underlineColor: 0xff00ff00)
    }

    private func canvas(_ cells: [NativeCell]) -> TerminalCanvas {
        let canvas = TerminalCanvas()
        var frame = TerminalGestureTests.frame(rows: 1, columns: UInt16(cells.count))
        frame.rows = [NativeRow(cells: cells, wrapped: false)]; frame.cursorVisible = false
        canvas.frame = CGRect(x: 0, y: 0, width: canvas.cellWidth * CGFloat(cells.count), height: canvas.cellHeight)
        canvas.update(frame)
        return canvas
    }

    private func pixels(_ canvas: TerminalCanvas) -> Data {
        let format = UIGraphicsImageRendererFormat(); format.scale = 1; format.opaque = true
        let image = UIGraphicsImageRenderer(bounds: canvas.bounds, format: format).image { _ in canvas.draw(canvas.bounds) }
        return image.pngData()!
    }

    func testHiddenTextAndDecorationsStayHiddenAndWideBackgroundOwnsContinuation() {
        let blank = pixels(canvas([cell()]))
        XCTAssertNotEqual(pixels(canvas([cell("M")])), blank)
        XCTAssertEqual(pixels(canvas([cell("M", attributes: 16 | 8, underline: 3)])), blank)
        XCTAssertNotEqual(pixels(canvas([cell(attributes: 8)])), blank, "strikethrough applies to spaces")
        var wide = cell(width: 2); wide.background = 0xffff0000
        var continuation = cell(width: 0); continuation.background = 0xff0000ff
        var half = cell(); half.background = 0xffff0000
        XCTAssertEqual(pixels(canvas([wide, continuation])), pixels(canvas([half, half])), "continuation cannot paint over its wide owner")
    }

    func testUnderlineStylesHaveDistinctInkIncludingEmptyCells() {
        let images = (0...5).map { style in pixels(canvas(Array(repeating: cell("", underline: UInt8(style)), count: 5))) }
        XCTAssertEqual(Set(images).count, 6, "none, single, double, curl, dots and dashes must render differently")
        for style in 1...5 {
            XCTAssertEqual(images[style], pixels(canvas(Array(repeating: cell(" ", underline: UInt8(style)), count: 5))))
        }
    }

    func testRepeatedFrameKeepsLayerAndChangedCellsRepaintMatchesFullDraw() throws {
        func rows(_ middle: [NativeCell]) -> [NativeRow] {
            [[cell("t"), cell("o"), cell("p")], middle, [cell("e"), cell("n"), cell("d")]].map { NativeRow(cells: $0, wrapped: false) }
        }
        let canvas = TerminalCanvas()
        var frame = TerminalGestureTests.frame(rows: 3, columns: 3)
        frame.rows = rows([cell("m"), cell("i"), cell("d")]); frame.cursorColumn = 1
        canvas.frame = CGRect(x: 0, y: 0, width: canvas.cellWidth * 3, height: canvas.cellHeight * 3)
        canvas.update(frame)
        canvas.layer.displayIfNeeded()
        canvas.update(frame)
        XCTAssertFalse(canvas.layer.needsDisplay(), "a repeated frame must keep the painted grid")

        var marked = cell("w"); marked.background = 0xffff0000
        var next = frame; next.rows = rows([cell("n"), marked, cell(" ", underline: 1)])
        next.cursorRow = 2
        let old = try XCTUnwrap(canvas.cursorRect)
        canvas.update(next)
        XCTAssertTrue(canvas.layer.needsDisplay())
        let moved = try XCTUnwrap(canvas.cursorRect)
        let full = pixels(canvas)
        // UIKit keeps unchanged pixels and clips drawing to whole pixels of the dirty
        // row and cursor cells. Cell edges here fall inside pixels at scale 1.
        let dirty = [CGRect(x: 0, y: canvas.cellHeight, width: canvas.bounds.width, height: canvas.cellHeight), old, moved].map(\.integral)
        for union in [false, true] {
            let format = UIGraphicsImageRendererFormat(); format.scale = 1; format.opaque = true
            let repainted = UIGraphicsImageRenderer(bounds: canvas.bounds, format: format).image { context in
                canvas.update(frame); canvas.draw(canvas.bounds)
                canvas.update(next)
                if union {
                    context.cgContext.clip(to: dirty)
                    canvas.draw(dirty.dropFirst().reduce(dirty[0]) { $0.union($1) })
                } else {
                    for rect in dirty {
                        context.cgContext.saveGState(); context.cgContext.clip(to: rect)
                        canvas.draw(rect)
                        context.cgContext.restoreGState()
                    }
                }
            }.pngData()
            XCTAssertEqual(repainted, full, union ? "bounding dirty rectangle" : "individual dirty rectangles")
        }
    }

    func testFontChangeDiscardsShapedGlyphs() throws {
        let resized = canvas([cell("M"), cell("中", width: 2), cell("", width: 0)])
        _ = pixels(resized)
        resized.font = TerminalFont.font(size: 20)
        let fresh = TerminalCanvas(); fresh.font = TerminalFont.font(size: 20)
        resized.frame = CGRect(x: 0, y: 0, width: fresh.cellWidth * 3, height: fresh.cellHeight)
        fresh.frame = resized.frame
        fresh.update(try XCTUnwrap(resized.nativeFrame))
        XCTAssertEqual(pixels(resized), pixels(fresh))
    }

    func testCursorCoversWideOwnerForBlockAndUnderlineButBeamStaysNarrow() throws {
        let canvas = canvas([cell("中", width: 2), cell("", width: 0)])
        var frame = try XCTUnwrap(canvas.nativeFrame)
        frame.cursorVisible = true; frame.cursorColumn = 1
        canvas.update(frame)
        XCTAssertEqual(canvas.cursorRect?.minX, 0)
        XCTAssertEqual(canvas.cursorRect?.width, canvas.cellWidth * 2)
        frame.cursorShape = .underline; canvas.update(frame)
        XCTAssertEqual(canvas.cursorRect?.width, canvas.cellWidth * 2)
        XCTAssertEqual(canvas.cursorRect?.height, 2)
        frame.cursorShape = .beam; canvas.update(frame)
        XCTAssertEqual(canvas.cursorRect?.width, 2)
        frame.cursorColumn = 2; canvas.update(frame)
        XCTAssertNil(canvas.cursorRect)
    }

    func testBlinkStopsDuringSelectionBackgroundAndRemoval() async throws {
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.first as? UIWindowScene)
        let window = UIWindow(windowScene: scene)
        window.rootViewController = UIViewController(); window.isHidden = false
        defer { window.isHidden = true }
        let canvas = canvas([cell()])
        window.rootViewController!.view.addSubview(canvas)
        var frame = try XCTUnwrap(canvas.nativeFrame); frame.cursorVisible = true; frame.cursorBlinking = true
        canvas.update(frame)
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while canvas.cursorPhaseVisible, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(canvas.cursorPhaseVisible)
        canvas.isSelecting = true; XCTAssertTrue(canvas.cursorPhaseVisible)
        try await Task.sleep(for: .milliseconds(600)); XCTAssertTrue(canvas.cursorPhaseVisible)
        canvas.isSelecting = false
        NotificationCenter.default.post(name: UIApplication.willResignActiveNotification, object: nil)
        try await Task.sleep(for: .milliseconds(600)); XCTAssertTrue(canvas.cursorPhaseVisible)
        NotificationCenter.default.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        canvas.removeFromSuperview()
        try await Task.sleep(for: .milliseconds(600)); XCTAssertTrue(canvas.cursorPhaseVisible)
    }

    func testOpenLinkOnlyAcceptsWebURLs() {
        XCTAssertNotNil(TerminalSelectionController.webURL("https://example.com/中文?q=x"))
        for value in [nil, "file:///tmp/a", "javascript:alert(1)", "zterm://host", "https:", "relative/path", "https:///" ] as [String?] {
            XCTAssertNil(TerminalSelectionController.webURL(value))
        }
    }
}
