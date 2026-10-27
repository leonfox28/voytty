import XCTest
import UIKit
@testable import Voytty

@MainActor
final class TerminalGestureTests: XCTestCase {
    static func frame(rows: UInt16 = 24, columns: UInt16 = 80) -> NativeFrame {
        NativeFrame(applicationTitle: "", connectionPath: .direct, rttMs: 1, pointerMode: .none, activeScreen: .main,
                    source: nil, stats: NativeNavigationStats(requests: 0, cacheHits: 0, cacheMisses: 0, retainedRows: 0, retainedBytes: 0, peakBytes: 0, queryRttMs: 0),
                    notice: nil, inputEpoch: 1, geometryGeneration: 1, inputReady: true, historyOffset: 0, windowOffset: 0, firstRow: 0,
                    contentGeneration: 0, selection: nil, generation: 1, state: "active", error: nil,
                    viewport: NativeViewport(rows: rows, columns: columns), rows: [], cursorRow: 0, cursorColumn: 0,
                    cursorVisible: true, cursorShape: .block, cursorBlinking: false, cursorColor: 0xff00ff00, background: 0xff000000, historyMaximum: 1000)
    }

    func testWheelPreservesPartialRowsReversalAndBoundsEachReport() {
        var wheel = TerminalWheel()
        XCTAssertEqual(wheel.move(to: 9, cellHeight: 20), 0)
        XCTAssertEqual(wheel.move(to: 39, cellHeight: 20), 1)
        XCTAssertEqual(wheel.move(to: 41, cellHeight: 20), 1)
        XCTAssertEqual(wheel.move(to: 5, cellHeight: 20), -1)
        XCTAssertEqual(wheel.move(to: -1, cellHeight: 20), -1)
        XCTAssertEqual(wheel.move(to: 2000, cellHeight: 20), 32)
        XCTAssertEqual(wheel.move(to: 2000, cellHeight: 20), 32)
        XCTAssertEqual(wheel.move(to: .infinity, cellHeight: 20), 0)
    }

    func testCoastingUsesElapsedTimeAndStopsAtBothHistoryEdges() {
        var sixty = TerminalHistoryMotion(position: 200, velocity: 100)
        var oneTwenty = sixty
        for _ in 0..<60 { _ = sixty.advance(seconds: 1.0 / 60, maximum: 1000) }
        for _ in 0..<120 { _ = oneTwenty.advance(seconds: 1.0 / 120, maximum: 1000) }
        XCTAssertEqual(sixty.position, oneTwenty.position, accuracy: 0.00001)
        XCTAssertEqual(sixty.velocity, oneTwenty.velocity, accuracy: 0.00001)
        var top = TerminalHistoryMotion(position: 999, velocity: 400)
        XCTAssertEqual(top.advance(seconds: 0.05, maximum: 1000), 1000)
        XCTAssertEqual(top.velocity, 0)
        var bottom = TerminalHistoryMotion(position: 1, velocity: -400)
        XCTAssertEqual(bottom.advance(seconds: 0.05, maximum: 1000), 0)
        XCTAssertEqual(bottom.velocity, 0)
        var paused = TerminalHistoryMotion(position: 0, velocity: 100)
        XCTAssertLessThan(paused.advance(seconds: 10, maximum: 1000), 10, "a delayed frame cannot jump by suspended wall time")
    }

    func testPointerStampRejectsChangedAuthorityGeometryModeAndHistory() {
        var frame = Self.frame(); frame.pointerMode = .mouse
        let stamp = TerminalGestureStamp(frame)
        XCTAssertTrue(stamp.accepts(frame))
        for change: (inout NativeFrame) -> Void in [
            { $0.inputEpoch += 1 }, { $0.geometryGeneration += 1 }, { $0.pointerMode = .alternateScroll },
            { $0.activeScreen = .alternate }, { $0.viewport.rows -= 1 }, { $0.historyOffset = 1 },
            { $0.inputReady = false }, { $0.state = "reconnecting" }
        ] {
            var changed = frame; change(&changed)
            XCTAssertFalse(stamp.accepts(changed))
        }
        frame.contentGeneration += 1; frame.generation += 1
        XCTAssertTrue(stamp.accepts(frame), "new text in the same grid does not change coordinate authority")
    }

    func testCellHitTestingExcludesPaddingAndUsesHostCellGeometry() {
        let size = NativeViewport(rows: 20, columns: 40), cell = CGSize(width: 8, height: 19)
        let hit = TerminalGeometry.cell(at: CGPoint(x: 319, y: 379), viewport: size, cell: cell)
        XCTAssertEqual(hit?.0, 19); XCTAssertEqual(hit?.1, 39)
        XCTAssertNil(TerminalGeometry.cell(at: CGPoint(x: 320, y: 1), viewport: size, cell: cell))
        XCTAssertNil(TerminalGeometry.cell(at: CGPoint(x: -1, y: 1), viewport: size, cell: cell))
        XCTAssertNil(TerminalGeometry.viewport(.zero, cell: cell))
        XCTAssertEqual(TerminalGeometry.viewport(CGSize(width: 321, height: 381), cell: cell), size)
        XCTAssertEqual(TerminalGeometry.viewport(CGSize(width: 10000, height: 10000), cell: cell), NativeViewport(rows: 80, columns: 240))
    }

    func testLateKeyboardHeightCannotReplaceCurrentGridButRotationCan() {
        let old = Self.frame(rows: 24)
        var late = old; late.viewport.rows = 18; late.geometryGeneration += 1
        let target = NativeViewport(rows: 12, columns: 80)
        XCTAssertTrue(TerminalGeometry.retainDisplayed(old, candidate: late, target: target, transitioning: true))
        XCTAssertTrue(TerminalGeometry.retainDisplayed(old, candidate: late, target: target, transitioning: false))
        late.viewport = target
        XCTAssertFalse(TerminalGeometry.retainDisplayed(old, candidate: late, target: target, transitioning: false))
        late.viewport.columns = 100
        XCTAssertFalse(TerminalGeometry.retainDisplayed(old, candidate: late, target: target, transitioning: true))
        late = old; late.viewport.rows = 12; late.inputEpoch += 1
        XCTAssertFalse(TerminalGeometry.retainDisplayed(old, candidate: late, target: target, transitioning: true))
    }

    func testKeyboardTransitionDefersViewportAndSelectionKeepsNativePriority() async throws {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 400, height: 800))
        let view = TerminalContainer(); window.addSubview(view)
        XCTAssertNotNil(view.window, "keyboard notifications apply only to an attached terminal")
        view.frame = CGRect(x: 0, y: 0, width: 320, height: 600)
        var requests: [NativeViewport] = []
        view.onSize = { requests.append($0) }
        view.layoutIfNeeded()
        await Task.yield()
        let full = try XCTUnwrap(requests.last)
        view.update(Self.frame(rows: full.rows, columns: full.columns), terminal: nil)
        XCTAssertTrue(view.gesturesReady)
        NotificationCenter.default.post(name: UIResponder.keyboardWillChangeFrameNotification, object: nil,
                                        userInfo: [UIResponder.keyboardAnimationDurationUserInfoKey: 0.3])
        view.frame.size.height = 500; view.layoutIfNeeded()
        view.frame.size.height = 320; view.layoutIfNeeded()
        await Task.yield()
        XCTAssertEqual(requests.count, 1)
        XCTAssertFalse(view.gesturesReady)
        NotificationCenter.default.post(name: UIResponder.keyboardDidChangeFrameNotification, object: nil)
        view.layoutIfNeeded(); await Task.yield()
        XCTAssertEqual(requests.count, 2)
        let compact = try XCTUnwrap(requests.last)
        XCTAssertLessThan(compact.rows, full.rows)
        XCTAssertEqual(compact.columns, full.columns)
        view.update(Self.frame(rows: compact.rows, columns: compact.columns), terminal: nil)
        XCTAssertTrue(view.gesturesReady)
        view.selection.onBegin?()
        XCTAssertFalse(view.gestures.gestureRecognizerShouldBegin(view.gestures.pan))
        view.selection.onEnd?()
        XCTAssertTrue(view.gestures.gestureRecognizerShouldBegin(view.gestures.pan))
        view.removeFromSuperview()
    }

    func testPointersShareOrderingAndRetainCapturedGeometryInInputQueue() async {
        let source = NativeFrameSource(noHandle: .init())
        var frame = Self.frame(); frame.pointerMode = .mouse
        let pointer = TerminalPointer(source: source, stamp: TerminalGestureStamp(frame), row: 2, column: 3)
        var sent: [TerminalInputQueue.Entry] = []
        let queue = TerminalInputQueue { sent.append($0) }
        queue.append(.text("before", modifiers: 0, paste: false), attachment: 1, epoch: 1)
        queue.append(.pointer(pointer), attachment: 1, epoch: 1, geometry: 7)
        queue.append(.text("after", modifiers: 0, paste: false), attachment: 1, epoch: 1)
        await queue.waitUntilDrained()
        XCTAssertEqual(sent.map(\.input), [.text("before", modifiers: 0, paste: false), .pointer(pointer), .text("after", modifiers: 0, paste: false)])
        XCTAssertEqual(sent[1].geometry, 7)
        queue.append(.pointer(pointer), attachment: 1, epoch: 1, geometry: 7)
        queue.cancel(); await queue.waitUntilDrained()
        XCTAssertEqual(sent.count, 3)
    }
}
