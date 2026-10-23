import UIKit

struct TerminalHardwareEvent: Equatable {
    var key: NativeKey
    var modifiers: UInt8
    var kind: UInt8 = 1
    var text: String = ""
    var epoch: UInt64
}

/// Translate terminal shortcuts, leaving ordinary text, dead keys, system
/// Command shortcuts and active IME composition with UIKit.
enum TerminalHardware {
    static func resolve(code: UIKeyboardHIDUsage, characters: String, base: String,
                        flags: UIKeyModifierFlags, composing: Bool, epoch: UInt64) -> TerminalHardwareEvent? {
        guard !composing, !flags.contains(.command) else { return nil }
        // iOS owns Control-Space for switching the active input language.
        if code == .keyboardSpacebar, flags.contains(.control) { return nil }
        let modifiers: UInt8 = (flags.contains(.shift) ? 1 : 0)
            | (flags.contains(.alternate) ? 2 : 0) | (flags.contains(.control) ? 4 : 0)
        let key: NativeKey
        switch code {
        case .keyboardEscape: key = .escape
        case .keyboardReturnOrEnter, .keypadEnter: key = .enter
        case .keyboardTab: key = .tab
        case .keyboardDeleteOrBackspace: key = .backspace
        case .keyboardDeleteForward: key = .delete
        case .keyboardLeftArrow: key = .left
        case .keyboardRightArrow: key = .right
        case .keyboardUpArrow: key = .up
        case .keyboardDownArrow: key = .down
        case .keyboardHome: key = .home
        case .keyboardEnd: key = .end
        case .keyboardPageUp: key = .pageUp
        case .keyboardPageDown: key = .pageDown
        case _ where (UIKeyboardHIDUsage.keyboardF1.rawValue ... UIKeyboardHIDUsage.keyboardF12.rawValue).contains(code.rawValue):
            key = .function(number: UInt8(code.rawValue - UIKeyboardHIDUsage.keyboardF1.rawValue + 1))
        default:
            guard modifiers & 6 != 0, !characters.isEmpty, base.unicodeScalars.count == 1,
                  let scalar = base.unicodeScalars.first, scalar.value >= 32, scalar.value != 127 else { return nil }
            key = .unicode(scalar: scalar.value)
        }
        var text = ""
        if case .unicode = key, modifiers & 4 == 0 {
            // Option is the terminal Alt modifier; don't turn Option-B into ∫.
            text = flags.contains(.shift) ? base.uppercased() : base
        }
        return TerminalHardwareEvent(key: key, modifiers: modifiers, text: text, epoch: epoch)
    }
}
