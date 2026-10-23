import UIKit

/// Read-only UITextInput. UIKit owns selection gestures, highlight, loupe, handles and edit menu.
/// TerminalInput remains the separate owner of keyboard/IME edits sent to the host.
final class TerminalSelectionView: UIView, UITextInput, UITextInteractionDelegate {
    let textInteraction = UITextInteraction(for: .nonEditable)
    private(set) var document = TerminalTextDocument()
    var cellSize = CGSize(width: 1, height: 1)
    var onSelection: ((NSRange?) -> Void)?
    var onCopy: (() -> Void)?
    var hyperlink: (() async -> URL?)?
    var onOpenLink: ((URL) -> Void)?
    var openLinkTitle = "Open Link"
    var onBegin: (() -> Void)?
    var onEnd: (() -> Void)?
    var onTouch: ((CGPoint?) -> Void)?
    var onEdgeTick: (() -> Void)?
    private(set) var visibleFirstRow: Int64 = 0
    private(set) var dragPoint: CGPoint?
    private var touchOrigin: CGPoint?
    private var hasDragged = false
    private var movingStart: Bool?
    private var edgeTimer: Timer?
    private let touchObserver = TerminalSelectionTouchObserver()
    private var selection: TerminalTextRange?
    private let noKeyboard = UIView(frame: .zero)
    weak var inputDelegate: (any UITextInputDelegate)?
    lazy var tokenizer: any UITextInputTokenizer = UITextInputStringTokenizer(textInput: self)
    var markedTextStyle: [NSAttributedString.Key: Any]?
    var markedTextRange: UITextRange? { nil }
    var textInputView: UIView { self }
    var beginningOfDocument: UITextPosition { document.position(at: 0) }
    var endOfDocument: UITextPosition { document.position(at: document.text.length) }
    var hasText: Bool { document.text.length > 0 }
    var hasSelection: Bool { selection?.isEmpty == false }
    override var canBecomeFirstResponder: Bool { true }
    override var inputView: UIView? { noKeyboard }

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .clear
        accessibilityIdentifier = "terminalSelection"
        textInteraction.textInput = self
        textInteraction.delegate = self
        addInteraction(textInteraction)
        touchObserver.changed = { [weak self] in self?.trackTouch($0) }
        touchObserver.began = { [weak self] in self?.onTouch?($0) }
        touchObserver.ended = { [weak self] in self?.onTouch?(nil) }
        addGestureRecognizer(touchObserver)
        NotificationCenter.default.addObserver(self, selector: #selector(stopTracking), name: UIApplication.willResignActiveNotification, object: nil)
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func update(rows: [NativeRow], firstRow: Int64 = 0) {
        guard !hasSelection else { return }
        inputDelegate?.textWillChange(self)
        document = TerminalTextDocument(rows: rows, firstRow: firstRow)
        visibleFirstRow = firstRow
        selection = nil
        inputDelegate?.textDidChange(self)
    }

    func show(rows: [NativeRow], firstRow: Int64, visibleFirst: Int64) throws {
        guard let selection, let range = document.range(selection), let endpoints = document.endpoints(range) else { return }
        // A soft-wrapped row-end position belongs to the following logical row.
        // Retain that row too so UIKit's exclusive endpoint stays valid.
        let merged = try document.merging(rows, first: firstRow,
                                          keeping: endpoints.0.row...max(endpoints.1.row, selection.upper.row))
        inputDelegate?.textWillChange(self)
        inputDelegate?.selectionWillChange(self)
        document = merged
        visibleFirstRow = visibleFirst
        // The stationary finger must keep extending across newly revealed rows.
        if hasDragged, let point = dragPoint, let movingStart {
            let clamped = CGPoint(x: max(0, min(bounds.maxX - 1, point.x)), y: max(0, min(bounds.maxY - 1, point.y)))
            if let position = closestPosition(to: clamped) {
                selectedTextRange = textRange(from: movingStart ? position : selection.start,
                                             to: movingStart ? selection.end : position)
            }
        }
        inputDelegate?.textDidChange(self)
        inputDelegate?.selectionDidChange(self)
    }

    func trackTouch(_ point: CGPoint?) {
        if let point {
            if let origin = touchOrigin {
                if hypot(point.x - origin.x, point.y - origin.y) >= 8 { hasDragged = true }
            } else { touchOrigin = point }
        }
        if dragPoint == nil, let point, let selection {
            let start = caretRect(for: selection.start), end = caretRect(for: selection.end)
            let a = hypot(point.x - start.midX, point.y - start.minY)
            let b = hypot(point.x - end.midX, point.y - end.maxY)
            movingStart = min(a, b) <= 44 ? a < b : nil
        }
        dragPoint = point
        if point == nil { stopTracking() }
        else { startEdgeTimer() }
    }

    private func startEdgeTimer() {
        // A stationary long press creates a word selection, even at the edge.
        // Only actual finger movement turns it into an extending drag.
        guard edgeTimer == nil, hasDragged, dragPoint != nil, movingStart != nil, hasSelection else { return }
        let timer = Timer(timeInterval: TerminalEdgeScroll.interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.onEdgeTick?() }
        }
        edgeTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    @objc func stopTracking() {
        edgeTimer?.invalidate()
        edgeTimer = nil
        dragPoint = nil
        touchOrigin = nil
        hasDragged = false
        movingStart = nil
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window == nil { stopTracking() }
    }

    func clear() {
        stopTracking()
        guard selection != nil else { return }
        inputDelegate?.selectionWillChange(self)
        selectedTextRange = nil
        inputDelegate?.selectionDidChange(self)
        if isFirstResponder { resignFirstResponder() }
    }

    var selectedTextRange: UITextRange? {
        get { selection }
        set {
            let range = (newValue as? TerminalTextRange).flatMap { document.range($0) }.map(document.normalized)
            guard range != selection.flatMap({ document.range($0) }) else { return }
            let old = selection
            selection = range.map(document.textRange)
            if dragPoint != nil, let next = selection {
                if let old, next.lower.matches(old.lower) { movingStart = false }
                else if let old, next.upper.matches(old.upper) { movingStart = true }
                else if let point = dragPoint {
                    movingStart = abs(point.y - caretRect(for: next.start).midY) < abs(point.y - caretRect(for: next.end).midY)
                }
            }
            onSelection?(range.flatMap { $0.length > 0 ? $0 : nil })
            startEdgeTimer()
        }
    }

    func interactionShouldBegin(_ interaction: UITextInteraction, at point: CGPoint) -> Bool { hasText }
    func interactionWillBegin(_ interaction: UITextInteraction) {
        onBegin?()
        if !isFirstResponder { becomeFirstResponder() }
    }
    func interactionDidEnd(_ interaction: UITextInteraction) { onEnd?() }

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        (action == #selector(copy(_:)) && hasSelection) || (action == #selector(selectAll(_:)) && hasText)
    }
    override func copy(_ sender: Any?) { if hasSelection { onCopy?() } }
    func editMenu(for textRange: UITextRange, suggestedActions: [UIMenuElement]) -> UIMenu? {
        let link = UIDeferredMenuElement.uncached { [weak self] completion in
            Task { @MainActor in
                guard let self, self.hasSelection, let url = await self.hyperlink?() else { completion([]); return }
                completion([UIAction(title: self.openLinkTitle, image: UIImage(systemName: "safari")) { [weak self] _ in
                    self?.onOpenLink?(url)
                }])
            }
        }
        return UIMenu(children: suggestedActions + [link])
    }
    override func selectAll(_ sender: Any?) {
        inputDelegate?.selectionWillChange(self)
        selectedTextRange = document.textRange(NSRange(location: 0, length: document.text.length))
        inputDelegate?.selectionDidChange(self)
    }

    func text(in range: UITextRange) -> String? {
        guard let range = range as? TerminalTextRange, let value = document.range(range) else { return nil }
        return document.text.substring(with: value)
    }
    func replace(_ range: UITextRange, withText text: String) {}
    func insertText(_ text: String) {}
    func deleteBackward() {}
    func setMarkedText(_ markedText: String?, selectedRange: NSRange) {}
    func unmarkText() {}

    func textRange(from: UITextPosition, to: UITextPosition) -> UITextRange? {
        let a = offset(from), b = offset(to)
        return document.textRange(NSRange(location: min(a, b), length: abs(b - a)))
    }
    func position(from position: UITextPosition, offset delta: Int) -> UITextPosition? {
        let value = offset(position) + delta
        return (0...document.text.length).contains(value) ? document.position(at: value) : nil
    }
    func position(from position: UITextPosition, in direction: UITextLayoutDirection, offset delta: Int) -> UITextPosition? {
        switch direction {
        case .left: return self.position(from: position, offset: -delta)
        case .right: return self.position(from: position, offset: delta)
        case .up, .down:
            let rect = caretRect(for: position)
            return closestPosition(to: CGPoint(x: rect.minX, y: rect.midY + CGFloat(direction == .up ? -delta : delta) * cellSize.height))
        @unknown default: return nil
        }
    }
    func compare(_ position: UITextPosition, to other: UITextPosition) -> ComparisonResult {
        offset(position) == offset(other) ? .orderedSame : offset(position) < offset(other) ? .orderedAscending : .orderedDescending
    }
    func offset(from: UITextPosition, to: UITextPosition) -> Int { offset(to) - offset(from) }
    func position(within range: UITextRange, farthestIn direction: UITextLayoutDirection) -> UITextPosition? {
        direction == .left || direction == .up ? range.start : range.end
    }
    func characterRange(byExtending position: UITextPosition, in direction: UITextLayoutDirection) -> UITextRange? {
        let index = offset(position) - (direction == .left || direction == .up ? 1 : 0)
        guard index >= 0, index < document.text.length else { return nil }
        return document.textRange(document.text.rangeOfComposedCharacterSequence(at: index))
    }
    func baseWritingDirection(for position: UITextPosition, in direction: UITextStorageDirection) -> NSWritingDirection { .leftToRight }
    func setBaseWritingDirection(_ writingDirection: NSWritingDirection, for range: UITextRange) {}
    func firstRect(for range: UITextRange) -> CGRect { selectionRects(for: range).first?.rect ?? caretRect(for: range.start) }
    func caretRect(for position: UITextPosition) -> CGRect { document.caret(at: offset(position), size: cellSize, visibleFirstRow: visibleFirstRow) }
    func selectionRects(for range: UITextRange) -> [UITextSelectionRect] {
        guard let range = range as? TerminalTextRange, let value = document.range(range) else { return [] }
        let boxes = document.rectangles(for: value, size: cellSize, visibleFirstRow: visibleFirstRow)
        return boxes.enumerated().map { TerminalSelectionRect($0.element, first: $0.offset == 0, last: $0.offset == boxes.count - 1) }
    }
    func closestPosition(to point: CGPoint) -> UITextPosition? {
        return document.position(at: document.closestOffset(to: point, size: cellSize, visibleFirstRow: visibleFirstRow))
    }
    func closestPosition(to point: CGPoint, within range: UITextRange) -> UITextPosition? {
        return document.position(at: max(offset(range.start), min(offset(range.end), document.closestOffset(to: point, size: cellSize, visibleFirstRow: visibleFirstRow))))
    }
    func characterRange(at point: CGPoint) -> UITextRange? {
        document.cell(at: point, size: cellSize, visibleFirstRow: visibleFirstRow).map { document.textRange($0.range) }
    }
    private func offset(_ position: UITextPosition) -> Int { (position as? TerminalTextPosition).flatMap { document.offset(of: $0) } ?? 0 }
}
