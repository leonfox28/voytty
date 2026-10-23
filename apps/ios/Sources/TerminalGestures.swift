import UIKit

/// Geometry and input authority of the cells the user actually touched.
struct TerminalGestureStamp: Equatable {
    let epoch: UInt64
    let geometry: UInt64
    let screen: NativeActiveScreen
    let mode: NativePointerMode
    let viewport: NativeViewport
    init(_ frame: NativeFrame) {
        epoch = frame.inputEpoch; geometry = frame.geometryGeneration
        screen = frame.activeScreen; mode = frame.pointerMode; viewport = frame.viewport
    }
    func accepts(_ frame: NativeFrame) -> Bool {
        mode != .none && frame.state == "active" && frame.inputReady && frame.historyOffset == 0 && frame.selection == nil
            && self == TerminalGestureStamp(frame)
    }
}

struct TerminalPointer: Equatable {
    let source: NativeFrameSource
    let stamp: TerminalGestureStamp
    let row: UInt16
    let column: UInt16
    var wheel: Int16 = 0
    static func == (a: Self, b: Self) -> Bool {
        a.source === b.source && a.stamp == b.stamp && a.row == b.row && a.column == b.column && a.wheel == b.wheel
    }
}

/// Fractional motion is retained; the shared terminal consumes bounded row steps.
struct TerminalWheel {
    private var previous: CGFloat = 0
    private var remainder: CGFloat = 0
    mutating func move(to translation: CGFloat, cellHeight: CGFloat) -> Int16 {
        guard translation.isFinite, cellHeight.isFinite, cellHeight > 0 else { return 0 }
        remainder += translation - previous; previous = translation
        let steps = Int16(max(-32, min(32, (remainder / cellHeight).rounded(.towardZero))))
        remainder -= CGFloat(steps) * cellHeight
        return steps
    }
}

/// Time-based decay is independent of 60/120 Hz display rates. Remote wheel
/// gestures never coast: one drag must not flood a child with cursor keys.
struct TerminalHistoryMotion {
    var position: Double
    var velocity: Double = 0
    mutating func advance(seconds: Double, maximum: UInt64) -> UInt64 {
        let elapsed = max(0, min(0.05, seconds))
        let decay = exp(-5 * elapsed)
        let next = position + velocity * (1 - decay) / 5
        position = max(0, min(Double(maximum), next))
        velocity *= decay
        if position != next || abs(velocity) < 0.5 { velocity = 0 }
        return UInt64(position.rounded())
    }
}

enum TerminalGeometry {
    static func viewport(_ bounds: CGSize, cell: CGSize) -> NativeViewport? {
        guard bounds.width.isFinite, bounds.height.isFinite, cell.width.isFinite, cell.height.isFinite,
              bounds.width > 0, bounds.height > 0, cell.width > 0, cell.height > 0 else { return nil }
        return NativeViewport(rows: UInt16(max(1, min(80, floor(bounds.height / cell.height)))),
                              columns: UInt16(max(1, min(240, floor(bounds.width / cell.width)))))
    }
    static func cell(at point: CGPoint, viewport: NativeViewport, cell: CGSize) -> (UInt16, UInt16)? {
        guard point.x.isFinite, point.y.isFinite, point.x >= 0, point.y >= 0, cell.width > 0, cell.height > 0,
              point.x < CGFloat(viewport.columns) * cell.width, point.y < CGFloat(viewport.rows) * cell.height else { return nil }
        return (UInt16(point.y / cell.height), UInt16(point.x / cell.width))
    }
    static func retainDisplayed(_ old: NativeFrame?, candidate: NativeFrame?, target: NativeViewport?, transitioning: Bool) -> Bool {
        guard let old, let next = candidate, old.viewport != next.viewport,
              transitioning || next.viewport != target,
              old.viewport.columns == next.viewport.columns,
              old.inputEpoch == next.inputEpoch, old.activeScreen == next.activeScreen,
              old.inputReady, next.inputReady, old.state == "active", next.state == "active",
              old.selection == nil, next.selection == nil, old.historyOffset == 0, next.historyOffset == 0 else { return false }
        return true
    }
}

@MainActor
final class TerminalGestures: NSObject, UIGestureRecognizerDelegate {
    private weak var view: TerminalContainer?
    private(set) var pan: UIPanGestureRecognizer!
    private var tapRecognizer: UITapGestureRecognizer!
    private var pointer: TerminalPointer?
    private var stamp: TerminalGestureStamp?
    private var wheel = TerminalWheel()
    private var history: TerminalHistoryMotion?
    private var origin: Double = 0
    private var lastOffset: UInt64?
    private var touching = false
    private var directTouch = false
    private var link: CADisplayLink?
    private var lastTick: CFTimeInterval?
    @MainActor private final class Tick: NSObject {
        weak var owner: TerminalGestures?
        @objc func fire(_ link: CADisplayLink) {
            if let owner { owner.tick(link) } else { link.invalidate() }
        }
    }

    init(view: TerminalContainer) {
        self.view = view
        super.init()
        pan = UIPanGestureRecognizer(target: self, action: #selector(drag(_:)))
        pan.allowedScrollTypesMask = .all; pan.maximumNumberOfTouches = 1; pan.delegate = self
        tapRecognizer = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
        tapRecognizer.delegate = self
        view.addGestureRecognizer(pan); view.addGestureRecognizer(tapRecognizer)
        for gesture in view.selection.textInteraction.gesturesForFailureRequirements {
            pan.require(toFail: gesture)
            // A native single tap focuses the read-only text input even without
            // selecting text. Give the remote mouse tap precedence over that
            // focus action, while UIKit's long press/double tap/handles still win.
            // Outside live mouse mode our delegate fails this tap immediately.
            if let tap = gesture as? UITapGestureRecognizer, tap.numberOfTapsRequired == 1 {
                gesture.require(toFail: tapRecognizer)
            } else {
                tapRecognizer.require(toFail: gesture)
            }
        }
        NotificationCenter.default.addObserver(self, selector: #selector(cancel), name: UIApplication.willResignActiveNotification, object: nil)
    }

    func touch(_ point: CGPoint?) {
        if let point, !touching { capture(point); directTouch = true }
        touching = point != nil
    }

    private func capture(_ point: CGPoint) {
        cancel()
        directTouch = false
        guard let view, view.gesturesReady, let frame = view.canvas.nativeFrame else { return }
        stamp = TerminalGestureStamp(frame)
        origin = Double(frame.historyOffset); lastOffset = frame.historyOffset
        if frame.historyOffset == 0, frame.pointerMode != .none, view.input.admitted,
           let cell = TerminalGeometry.cell(at: point, viewport: frame.viewport, cell: view.cellSize),
           let source = try? frame.source?.viewportSource(firstRow: view.canvas.visibleFirstRow) {
            pointer = TerminalPointer(source: source, stamp: TerminalGestureStamp(frame), row: cell.0, column: cell.1)
        } else if frame.activeScreen == .main {
            history = TerminalHistoryMotion(position: origin)
        }
    }

    @objc func cancel() {
        pointer = nil; stamp = nil; history = nil; wheel = TerminalWheel()
        lastOffset = nil; lastTick = nil
        link?.invalidate(); link = nil
        view?.selectionController?.cancelScrolling()
    }

    func update(_ frame: NativeFrame?) {
        guard let stamp else { return }
        guard let view, view.gesturesReady, let frame,
              stamp.epoch == frame.inputEpoch, stamp.geometry == frame.geometryGeneration,
              stamp.screen == frame.activeScreen, stamp.mode == frame.pointerMode,
              pointer == nil || stamp.accepts(frame) && view.input.admitted else { cancel(); return }
    }

    @objc private func tapped(_ gesture: UITapGestureRecognizer) {
        guard gesture.state == .ended else { return }
        tap()
    }

    func tap() {
        guard let view, view.gesturesReady, let frame = view.canvas.nativeFrame else { return }
        defer { pointer = nil; stamp = nil }
        guard let pointer, pointer.stamp.mode == .mouse, pointer.stamp.accepts(frame), view.input.admitted else { return }
        view.onPointer?(pointer)
    }

    @objc private func drag(_ gesture: UIPanGestureRecognizer) {
        guard let view, view.gesturesReady else { cancel(); return }
        if gesture.state == .began, gesture.numberOfTouches == 0 { capture(gesture.location(in: view)) }
        guard let frame = view.canvas.nativeFrame else { cancel(); return }
        update(frame)
        guard stamp != nil else { return }
        move(to: gesture.translation(in: view).y)
        switch gesture.state {
        case .ended: end(velocity: gesture.velocity(in: view).y)
        case .cancelled, .failed: cancel()
        default: break
        }
    }

    func move(to translation: CGFloat) {
        guard let view, view.gesturesReady, let frame = view.canvas.nativeFrame else { cancel(); return }
        update(frame)
        guard stamp != nil else { return }
        if var pointer {
            let lines = wheel.move(to: translation, cellHeight: view.cellSize.height)
            if lines != 0 { pointer.wheel = lines; view.onPointer?(pointer) }
        } else if history != nil {
            history?.position = max(0, min(Double(frame.historyMaximum), origin + Double(translation / view.cellSize.height)))
            submit(UInt64(history!.position.rounded()))
        }
    }

    func end(velocity: CGFloat) {
        guard let view else { cancel(); return }
        if history != nil, directTouch {
            let rows = Double(velocity / view.cellSize.height)
            history?.velocity = max(-400, min(400, rows))
            if abs(rows) > 2 { startCoasting() } else { history = nil; stamp = nil }
        } else { pointer = nil; stamp = nil }
    }

    private func submit(_ offset: UInt64) {
        guard offset != lastOffset else { return }
        let older = offset > (lastOffset ?? 0)
        lastOffset = offset
        view?.selectionController?.scroll(offset: offset, older: older)
    }

    private func startCoasting() {
        let target = Tick(); target.owner = self
        let display = CADisplayLink(target: target, selector: #selector(Tick.fire(_:)))
        display.preferredFrameRateRange = CAFrameRateRange(minimum: 30, maximum: 120, preferred: 60)
        link = display
        display.add(to: .main, forMode: .common)
    }

    private func tick(_ link: CADisplayLink) {
        guard let view, let frame = view.canvas.nativeFrame else { cancel(); return }
        update(frame)
        guard history != nil else { return }
        let now = link.targetTimestamp
        let elapsed = lastTick.map { now - $0 } ?? link.targetTimestamp - link.timestamp
        lastTick = now
        let offset = history!.advance(seconds: elapsed, maximum: frame.historyMaximum)
        submit(offset)
        if history?.velocity == 0 { self.link?.invalidate(); self.link = nil; lastTick = nil }
    }

    func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        guard let view, view.gesturesReady else { return false }
        if gestureRecognizer === tapRecognizer { return view.input.admitted && view.canvas.nativeFrame?.pointerMode == .mouse && view.canvas.nativeFrame?.historyOffset == 0 }
        return true
    }
    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldReceive touch: UITouch) -> Bool {
        var target = touch.view
        while let current = target, current !== view {
            if current is UIControl { return false }
            target = current.superview
        }
        return true
    }
}
