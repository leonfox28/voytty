import UIKit
import UIKit.UIGestureRecognizerSubclass

/// Observes the touch stream without recognizing a competing gesture or cancelling UIKit's handles.
final class TerminalSelectionTouchObserver: UIGestureRecognizer {
    var changed: ((CGPoint?) -> Void)?
    var began: ((CGPoint) -> Void)?
    var ended: (() -> Void)?
    private weak var trackedTouch: UITouch?

    init() {
        super.init(target: nil, action: nil)
        cancelsTouchesInView = false
        delaysTouchesBegan = false
        delaysTouchesEnded = false
    }
    override func canPrevent(_ preventedGestureRecognizer: UIGestureRecognizer) -> Bool { false }
    override func canBePrevented(by preventingGestureRecognizer: UIGestureRecognizer) -> Bool { false }
    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        if trackedTouch == nil {
            trackedTouch = touches.first
            if let touch = trackedTouch { began?(touch.location(in: view)) }
        }
        if let touch = trackedTouch { changed?(touch.location(in: view)) }
    }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) {
        if let touch = trackedTouch { changed?(touch.location(in: view)) }
    }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
        if let touch = trackedTouch, touches.contains(touch) { ended?(); changed?(nil); state = .failed }
    }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) { ended?(); changed?(nil); state = .failed }
    override func reset() { super.reset(); trackedTouch = nil; ended?(); changed?(nil) }
}

/// One bounded row step per tick; wait for that viewport before requesting another.
struct TerminalEdgeScroll {
    static let interval: TimeInterval = 1.0 / 12
    static func direction(point: CGPoint, bounds: CGRect, edge: CGFloat = 36) -> Int {
        guard bounds.height > edge * 2, point.x >= bounds.minX - edge, point.x <= bounds.maxX + edge else { return 0 }
        if point.y < bounds.minY + edge { return 1 }
        if point.y > bounds.maxY - edge { return -1 }
        return 0
    }
    static func target(offset: UInt64, maximum: UInt64, direction: Int) -> UInt64? {
        if direction > 0, offset < maximum { return offset + 1 }
        if direction < 0, offset > 0 { return offset - 1 }
        return nil
    }
}
