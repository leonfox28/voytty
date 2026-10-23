import UIKit

/// UIKit owns marked text; only committed edits cross the native input boundary.
final class TerminalInput: UITextView, UITextViewDelegate {
    var onText: ((String, Bool) -> Void)?
    var onKey: ((NativeKey) -> Void)?
    var onFocusChange: ((Bool) -> Void)?
    var onHardware: ((TerminalHardwareEvent) -> Bool)?
    var pendingModifiers: (() -> UInt8)?
    var onCompositionChange: (() -> Void)?
    var admitted = false
    private var lastEpoch: UInt64?
    private var held: [UIKeyboardHIDUsage: TerminalHardwareEvent] = [:]
    private var owned: Set<UIKeyboardHIDUsage> = []
    private let hiddenKeyboard = UIView(frame: .zero)
    private(set) var softwareKeyboard = false

    init() {
        super.init(frame: .zero, textContainer: nil)
        delegate = self
        backgroundColor = .clear
        // Committed text is cleared synchronously by the delegate. Keep native
        // marked text visible while the IME owns the uncommitted composition.
        textColor = .label
        tintColor = .clear
        autocorrectionType = .no
        autocapitalizationType = .none
        smartQuotesType = .no
        smartDashesType = .no
        smartInsertDeleteType = .no
        spellCheckingType = .no
        keyboardAppearance = .default
        textContentType = .none
        inputView = hiddenKeyboard
        inputAssistantItem.leadingBarButtonGroups = []
        inputAssistantItem.trailingBarButtonGroups = []
        textContainerInset = .zero
        textContainer.lineFragmentPadding = 0
        isScrollEnabled = false
        markedTextStyle = [.foregroundColor: UIColor.label, .backgroundColor: UIColor.secondarySystemBackground,
                           .underlineStyle: NSUnderlineStyle.single.rawValue]
        accessibilityLabel = "终端输入"
        accessibilityIdentifier = "terminalInput"
        NotificationCenter.default.addObserver(self, selector: #selector(suspendInput),
                                               name: UIApplication.willResignActiveNotification, object: nil)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    // The canvas' read-only UITextInteraction owns touch selection, including
    // the cells beneath this keyboard/IME endpoint.
    override func point(inside point: CGPoint, with event: UIEvent?) -> Bool { false }

    func admission(ready: Bool, epoch: UInt64?) {
        if lastEpoch != epoch || !ready || !admitted {
            // Discard compositions made before or during an input/upload pause,
            // including ones committed after admission resumes in the same epoch.
            discardComposition()
            held.removeAll()
        }
        lastEpoch = epoch
        admitted = ready
    }

    func setSoftwareKeyboard(_ visible: Bool) {
        guard softwareKeyboard != visible else { return }
        softwareKeyboard = visible
        inputView = visible ? nil : hiddenKeyboard
        if isFirstResponder { reloadInputViews() }
    }

    private var discarding = false
    func discardComposition() {
        discarding = true
        text = ""
        super.unmarkText()
        discarding = false
        onCompositionChange?()
    }

    override func setMarkedText(_ markedText: String?, selectedRange: NSRange) {
        guard admitted else { return }
        super.setMarkedText(markedText, selectedRange: selectedRange)
        onCompositionChange?()
    }

    override func unmarkText() {
        super.unmarkText()
        onCompositionChange?()
    }

    func textViewDidChange(_ textView: UITextView) {
        guard !discarding, markedTextRange == nil else { return }
        let committed = text ?? ""
        text = ""
        if admitted && !committed.isEmpty { onText?(committed, false) }
    }

    func textViewDidBeginEditing(_ textView: UITextView) { onFocusChange?(softwareKeyboard) }
    func textViewDidEndEditing(_ textView: UITextView) {
        releaseHeldKeys()
        discardComposition()
        onFocusChange?(false)
    }

    func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
        if text == "\n", markedTextRange == nil {
            if admitted { onKey?(.enter) }
            return false
        }
        return true
    }

    override func deleteBackward() {
        if markedTextRange == nil && (text ?? "").isEmpty {
            if admitted { onKey?(.backspace) }
        } else { super.deleteBackward() }
    }

    override func paste(_ sender: Any?) {
        guard admitted, let value = UIPasteboard.general.string, !value.isEmpty else { return }
        discardComposition()
        onText?(value, true)
    }

    // Keep UIKit's normal text/IME path. Only terminal shortcuts are intercepted.
    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let remaining = presses.filter { press in
            guard let key = press.key else { return true }
            return !hardwareBegan(code: key.keyCode, characters: key.characters,
                                  base: key.charactersIgnoringModifiers, flags: key.modifierFlags)
        }
        if !remaining.isEmpty { super.pressesBegan(Set(remaining), with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let remaining = presses.filter { $0.key.map { !hardwareEnded(code: $0.keyCode) } ?? true }
        if !remaining.isEmpty { super.pressesEnded(Set(remaining), with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let remaining = presses.filter { $0.key.map { !hardwareEnded(code: $0.keyCode) } ?? true }
        if !remaining.isEmpty { super.pressesCancelled(Set(remaining), with: event) }
    }

    @discardableResult
    func hardwareBegan(code: UIKeyboardHIDUsage, characters: String, base: String, flags: UIKeyModifierFlags) -> Bool {
        if owned.contains(code) {
            if var event = held[code], admitted, event.epoch == lastEpoch {
                event.kind = 2
                _ = onHardware?(event)
            }
            return true
        }
        guard var event = TerminalHardware.resolve(code: code, characters: characters, base: base,
                flags: flags, composing: markedTextRange != nil, epoch: lastEpoch ?? 0) else { return false }
        owned.insert(code)
        guard admitted else { return true }
        event.modifiers |= pendingModifiers?() ?? 0
        if onHardware?(event) == true { held[code] = event }
        return true
    }

    @discardableResult
    func hardwareEnded(code: UIKeyboardHIDUsage) -> Bool {
        guard owned.remove(code) != nil else { return false }
        if var event = held.removeValue(forKey: code), admitted, event.epoch == lastEpoch {
            event.kind = 3; event.text = ""
            _ = onHardware?(event)
        }
        return true
    }

    func releaseHeldKeys() {
        for code in Array(owned) { hardwareEnded(code: code) }
    }

    @objc private func suspendInput() {
        releaseHeldKeys()
        discardComposition()
    }
}
