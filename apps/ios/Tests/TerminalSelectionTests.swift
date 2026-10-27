import XCTest
import UIKit
@testable import Voytty

final class TerminalSelectionTests: XCTestCase {
    private func cell(_ text: String, _ width: UInt8 = 1) -> NativeCell {
        NativeCell(text: text, width: width, foreground: 0xffffffff, background: 0xff000000,
                   attributes: 0, underline: 0, underlineColor: 0xffffffff)
    }

    @MainActor
    func testUnicodeSelectionUsesUTF16AndWholeTerminalCells() {
        let document = TerminalTextDocument(rows: [NativeRow(cells: [cell("a"), cell("中", 2), cell("", 0),
            cell("👩🏽‍💻", 2), cell("", 0), cell("e\u{301}")], wrapped: false)])
        XCTAssertEqual(document.text as String, "a中👩🏽‍💻e\u{301}")
        let emoji = (document.text as String as NSString).range(of: "👩🏽‍💻")
        let insideSurrogate = NSRange(location: emoji.location + 1, length: 1)
        XCTAssertEqual(document.normalized(insideSurrogate), emoji)
        let size = CGSize(width: 10, height: 20)
        XCTAssertEqual(document.cell(at: CGPoint(x: 24, y: 10), size: size)?.range, NSRange(location: 1, length: 1))
        XCTAssertEqual(document.rectangles(for: emoji, size: size), [CGRect(x: 30, y: 0, width: 20, height: 20)])
        XCTAssertEqual(document.normalized(NSRange(location: document.text.length - 1, length: 1)).length, 2)
    }

    @MainActor
    func testSoftWrapHardBreakAndReverseRangeGeometry() {
        let view = TerminalSelectionView()
        view.cellSize = CGSize(width: 10, height: 20)
        view.update(rows: [NativeRow(cells: [cell("a"), cell("b")], wrapped: true),
                           NativeRow(cells: [cell("c"), cell("d")], wrapped: false),
                           NativeRow(cells: [cell("e"), cell("")], wrapped: false)])
        XCTAssertEqual(view.document.text as String, "abcd\ne ")
        let range = view.textRange(from: view.document.position(at: 6), to: view.document.position(at: 1))!
        XCTAssertEqual(view.text(in: range), "bcd\ne")
        let rects = view.selectionRects(for: range)
        XCTAssertEqual(rects.map(\.rect), [CGRect(x: 10, y: 0, width: 10, height: 20),
            CGRect(x: 0, y: 20, width: 20, height: 20), CGRect(x: 0, y: 40, width: 10, height: 20)])
        XCTAssertTrue(rects.first!.containsStart)
        XCTAssertTrue(rects.last!.containsEnd)
        XCTAssertEqual(view.offset(from: view.beginningOfDocument, to: view.closestPosition(to: CGPoint(x: 4, y: 45))!), 5)
    }

    @MainActor
    func testNativeInteractionIsReadOnlyAndPinsSelectedDocument() {
        let view = TerminalSelectionView()
        view.update(rows: [NativeRow(cells: [cell("中", 2), cell("", 0), cell("x")], wrapped: false)])
        var copies = 0
        var selections: [NSRange?] = []
        view.onCopy = { copies += 1 }
        view.onSelection = { selections.append($0) }
        XCTAssertTrue(view.interactions.contains { $0 === view.textInteraction })
        XCTAssertEqual(view.textInteraction.textInteractionMode, .nonEditable)
        view.selectedTextRange = view.document.textRange(NSRange(location: 0, length: 1))
        view.update(rows: [NativeRow(cells: [cell("z")], wrapped: false)])
        view.insertText("must not edit")
        view.deleteBackward()
        view.replace(view.selectedTextRange!, withText: "must not edit")
        XCTAssertEqual(view.document.text as String, "中x")
        XCTAssertTrue(view.canPerformAction(#selector(UIResponderStandardEditActions.copy(_:)), withSender: nil))
        XCTAssertFalse(view.canPerformAction(#selector(UIResponderStandardEditActions.cut(_:)), withSender: nil))
        XCTAssertFalse(view.canPerformAction(#selector(UIResponderStandardEditActions.paste(_:)), withSender: nil))
        view.copy(nil)
        XCTAssertEqual(copies, 1)
        view.clear()
        XCTAssertNil(selections.last!)
        view.update(rows: [NativeRow(cells: [cell("z")], wrapped: false)])
        XCTAssertEqual(view.document.text as String, "z")
        XCTAssertFalse(view.hasSelection)
    }

    @MainActor
    func testPrependingHistoryPreservesUIKitPositionsAndOffscreenAnchor() throws {
        let view = TerminalSelectionView()
        let a = NativeRow(cells: [cell("中", 2), cell("", 0)], wrapped: false)
        let b = NativeRow(cells: [cell("👩🏽‍💻", 2), cell("", 0)], wrapped: false)
        view.cellSize = CGSize(width: 10, height: 20)
        view.frame = CGRect(x: 0, y: 0, width: 20, height: 40)
        view.update(rows: [a, b], firstRow: 100)
        let originalStart = view.document.position(at: 0)
        let originalEnd = view.document.position(at: view.document.text.length)
        view.selectedTextRange = view.textRange(from: originalStart, to: originalEnd)
        let older = NativeRow(cells: [cell("e\u{301}"), cell("x")], wrapped: true)
        try view.show(rows: [older, a], firstRow: 99, visibleFirst: 99)
        XCTAssertEqual(view.text(in: view.selectedTextRange!), "中\n👩🏽‍💻")
        XCTAssertEqual(view.compare(originalStart, to: view.selectedTextRange!.start), .orderedSame)
        XCTAssertEqual(view.caretRect(for: originalStart).minY, 20)
        XCTAssertEqual(view.caretRect(for: originalEnd).minY, 40)
        XCTAssertEqual(view.offset(from: view.beginningOfDocument, to: originalStart), 3)
    }

    @MainActor
    func testSoftWrapExclusiveEndpointSurvivesHistoryPrepending() throws {
        let view = TerminalSelectionView()
        let wrapped = NativeRow(cells: [cell("a"), cell("b")], wrapped: true)
        let next = NativeRow(cells: [cell("c"), cell("d")], wrapped: false)
        view.update(rows: [wrapped, next], firstRow: 10)
        view.selectedTextRange = view.document.textRange(NSRange(location: 0, length: 2))
        let endpoint = view.selectedTextRange!.end
        XCTAssertEqual((endpoint as? TerminalTextPosition)?.row, 11)
        try view.show(rows: [wrapped, wrapped], firstRow: 9, visibleFirst: 9)
        XCTAssertEqual(view.text(in: view.selectedTextRange!), "ab")
        XCTAssertEqual(view.compare(endpoint, to: view.selectedTextRange!.end), .orderedSame)
        XCTAssertEqual(view.offset(from: view.beginningOfDocument, to: endpoint), 4)
    }

    @MainActor
    func testLongPressAtEdgeDoesNotScrollUntilFingerDragsAndStopsAfterLift() async throws {
        let view = TerminalSelectionView()
        view.frame = CGRect(x: 0, y: 0, width: 200, height: 400)
        view.cellSize = CGSize(width: 10, height: 20)
        view.update(rows: [NativeRow(cells: "COPY_CHECK".map { cell(String($0)) }, wrapped: false)])
        var ticks = 0
        view.onEdgeTick = { ticks += 1 }
        // UIKit establishes its native word selection while the initial finger
        // is still down. Small long-press jitter must not start edge scrolling.
        view.trackTouch(CGPoint(x: 40, y: 10))
        view.selectedTextRange = view.document.textRange(NSRange(location: 0, length: 10))
        view.trackTouch(CGPoint(x: 42, y: 9))
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(ticks, 0)
        XCTAssertEqual(view.text(in: view.selectedTextRange!), "COPY_CHECK")
        view.trackTouch(CGPoint(x: 40, y: 0))
        let deadline = ContinuousClock.now.advanced(by: .seconds(1))
        while ticks == 0, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertGreaterThan(ticks, 0, "a real drag held at the edge must keep scrolling")
        view.trackTouch(nil)
        let stopped = ticks
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(ticks, stopped)
        // A new gesture must not inherit the previous drag's eligibility.
        view.clear()
        view.trackTouch(CGPoint(x: 40, y: 10))
        view.selectedTextRange = view.document.textRange(NSRange(location: 0, length: 10))
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(ticks, stopped)
        view.stopTracking()
    }

    @MainActor
    func testStationaryFingerExtendsAcrossNewRowsAndStopsAfterLift() throws {
        let view = TerminalSelectionView()
        func row(_ value: String) -> NativeRow { NativeRow(cells: [cell(value), cell("x")], wrapped: false) }
        view.frame = CGRect(x: 0, y: 0, width: 20, height: 80)
        view.cellSize = CGSize(width: 10, height: 20)
        view.update(rows: [row("a"), row("b"), row("c"), row("d")], firstRow: 10)
        view.selectedTextRange = view.document.textRange(NSRange(location: 3, length: 4))
        // Start on the leading native handle, then hold at the upper edge.
        view.trackTouch(CGPoint(x: 0, y: 20))
        view.trackTouch(CGPoint(x: 0, y: 0))
        try view.show(rows: [row("z"), row("a"), row("b"), row("c")], firstRow: 9, visibleFirst: 9)
        XCTAssertEqual(view.text(in: view.selectedTextRange!), "zx\nax\nbx\nc")
        view.trackTouch(nil)
        try view.show(rows: [row("y"), row("z"), row("a"), row("b")], firstRow: 8, visibleFirst: 8)
        XCTAssertEqual(view.text(in: view.selectedTextRange!), "zx\nax\nbx\nc")
        XCTAssertNil(view.dragPoint)
        // The trailing native handle can extend toward newer rows as well.
        view.clear()
        view.update(rows: [row("a"), row("b"), row("c"), row("d")], firstRow: 10)
        view.selectedTextRange = view.document.textRange(NSRange(location: 3, length: 4))
        view.trackTouch(CGPoint(x: 10, y: 60))
        view.trackTouch(CGPoint(x: 20, y: 80))
        try view.show(rows: [row("b"), row("c"), row("d"), row("e")], firstRow: 11, visibleFirst: 11)
        XCTAssertEqual(view.text(in: view.selectedTextRange!), "bx\ncx\ndx\nex")
        view.stopTracking()
    }

    @MainActor
    func testHistoryMergeRejectsGapsChangesAndResourceOverflow() {
        let row = NativeRow(cells: [cell("a")], wrapped: false)
        let document = TerminalTextDocument(rows: [row], firstRow: 10)
        XCTAssertThrowsError(try document.merging([row], first: 12, keeping: 10...10))
        XCTAssertThrowsError(try document.merging([NativeRow(cells: [cell("b")], wrapped: false)], first: 10, keeping: 10...10))
        XCTAssertThrowsError(try document.merging([row], first: 5000, keeping: 10...10))
        let large = NativeRow(cells: Array(repeating: cell("x"), count: TerminalTextDocument.maximumCells + 1), wrapped: false)
        XCTAssertThrowsError(try document.merging([large], first: 11, keeping: 10...10))
    }

    func testEdgeScrollIsBoundedAndInactiveAwayFromEdges() {
        let bounds = CGRect(x: 0, y: 0, width: 300, height: 500)
        XCTAssertEqual(TerminalEdgeScroll.direction(point: CGPoint(x: 100, y: 4), bounds: bounds), 1)
        XCTAssertEqual(TerminalEdgeScroll.direction(point: CGPoint(x: 100, y: 496), bounds: bounds), -1)
        XCTAssertEqual(TerminalEdgeScroll.direction(point: CGPoint(x: 100, y: 250), bounds: bounds), 0)
        XCTAssertEqual(TerminalEdgeScroll.direction(point: CGPoint(x: -100, y: 4), bounds: bounds), 0)
        XCTAssertNil(TerminalEdgeScroll.target(offset: 0, maximum: 100, direction: -1))
        XCTAssertNil(TerminalEdgeScroll.target(offset: 100, maximum: 100, direction: 1))
        XCTAssertEqual(TerminalEdgeScroll.target(offset: 50, maximum: 100, direction: 1), 51)
        XCTAssertEqual(TerminalEdgeScroll.target(offset: 50, maximum: 100, direction: -1), 49)
    }
}
