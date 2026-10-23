import XCTest
import UIKit
@testable import Zterm

@MainActor
final class TerminalHardwareTests: XCTestCase {
    func testNavigationFunctionAndModifierMapping() {
        let pairs: [(UIKeyboardHIDUsage, NativeKey)] = [
            (.keyboardEscape, .escape), (.keyboardReturnOrEnter, .enter), (.keypadEnter, .enter),
            (.keyboardTab, .tab), (.keyboardDeleteOrBackspace, .backspace), (.keyboardDeleteForward, .delete),
            (.keyboardLeftArrow, .left), (.keyboardRightArrow, .right), (.keyboardUpArrow, .up), (.keyboardDownArrow, .down),
            (.keyboardHome, .home), (.keyboardEnd, .end), (.keyboardPageUp, .pageUp), (.keyboardPageDown, .pageDown),
            (.keyboardF1, .function(number: 1)), (.keyboardF12, .function(number: 12))]
        for (code, key) in pairs {
            XCTAssertEqual(TerminalHardware.resolve(code: code, characters: "", base: "", flags: [.control, .alternate, .shift], composing: false, epoch: 7),
                           TerminalHardwareEvent(key: key, modifiers: 7, epoch: 7))
        }
        XCTAssertEqual(TerminalHardware.resolve(code: .keyboardC, characters: "\u{3}", base: "c", flags: .control, composing: false, epoch: 7),
                       TerminalHardwareEvent(key: .unicode(scalar: 99), modifiers: 4, epoch: 7))
        XCTAssertEqual(TerminalHardware.resolve(code: .keyboardB, characters: "∫", base: "b", flags: .alternate, composing: false, epoch: 7)?.text, "b")
    }

    func testUIKitOwnsCompositionDeadKeysOrdinaryTextAndCommandShortcuts() {
        for code in [UIKeyboardHIDUsage.keyboardReturnOrEnter, .keyboardTab, .keyboardLeftArrow, .keyboardEscape, .keyboardDeleteOrBackspace] {
            XCTAssertNil(TerminalHardware.resolve(code: code, characters: "", base: "", flags: [], composing: true, epoch: 1))
        }
        XCTAssertNil(TerminalHardware.resolve(code: .keyboardE, characters: "", base: "e", flags: .alternate, composing: false, epoch: 1))
        XCTAssertNil(TerminalHardware.resolve(code: .keyboardA, characters: "A", base: "a", flags: .shift, composing: false, epoch: 1))
        XCTAssertNil(TerminalHardware.resolve(code: .keyboardV, characters: "v", base: "v", flags: .command, composing: false, epoch: 1))
        XCTAssertNil(TerminalHardware.resolve(code: .keyboardSpacebar, characters: " ", base: " ", flags: .control, composing: false, epoch: 1))
    }

    func testPressRepeatReleaseRetainsLatchedModifiersAndEpoch() {
        let input = TerminalInput()
        input.admission(ready: true, epoch: 9)
        var events: [TerminalHardwareEvent] = []
        var latch: UInt8 = 4
        input.pendingModifiers = { latch }
        input.onHardware = { events.append($0); latch = 0; return true }
        XCTAssertTrue(input.hardwareBegan(code: .keyboardLeftArrow, characters: "", base: "", flags: .alternate))
        XCTAssertTrue(input.hardwareBegan(code: .keyboardLeftArrow, characters: "", base: "", flags: []))
        XCTAssertTrue(input.hardwareEnded(code: .keyboardLeftArrow))
        XCTAssertFalse(input.hardwareEnded(code: .keyboardLeftArrow))
        XCTAssertEqual(events.map(\.kind), [1, 2, 3])
        XCTAssertEqual(events.map(\.modifiers), [6, 6, 6])
        XCTAssertEqual(events.map(\.epoch), [9, 9, 9])
    }

    func testAdmissionPauseAndNewEpochCannotReplayHeldKeys() {
        let input = TerminalInput()
        input.admission(ready: true, epoch: 1)
        var events: [TerminalHardwareEvent] = []
        input.onHardware = { events.append($0); return true }
        input.hardwareBegan(code: .keyboardUpArrow, characters: "", base: "", flags: [])
        input.admission(ready: false, epoch: 1)
        input.admission(ready: true, epoch: 1)
        input.hardwareBegan(code: .keyboardUpArrow, characters: "", base: "", flags: [])
        input.hardwareEnded(code: .keyboardUpArrow)
        XCTAssertEqual(events.count, 1, "pause must retire repeats/releases even when the epoch stays the same")
        input.hardwareBegan(code: .keyboardDownArrow, characters: "", base: "", flags: [])
        input.admission(ready: true, epoch: 2)
        input.hardwareEnded(code: .keyboardDownArrow)
        XCTAssertEqual(events.count, 2)
        input.hardwareBegan(code: .keyboardUpArrow, characters: "", base: "", flags: [])
        input.releaseHeldKeys()
        XCTAssertEqual(events.suffix(2).map(\.kind), [1, 3])
        XCTAssertEqual(events.last?.epoch, 2)
    }

    func testRejectedPressDoesNotProduceRepeatOrRelease() {
        let input = TerminalInput()
        input.admission(ready: true, epoch: 1)
        var attempts = 0
        input.onHardware = { _ in attempts += 1; return false }
        input.hardwareBegan(code: .keyboardTab, characters: "", base: "", flags: [])
        input.hardwareBegan(code: .keyboardTab, characters: "", base: "", flags: [])
        input.hardwareEnded(code: .keyboardTab)
        XCTAssertEqual(attempts, 1)
    }

    func testChineseCompositionReplacementDeletionCommitAndPause() {
        let input = TerminalInput()
        input.admission(ready: true, epoch: 1)
        var texts: [String] = [], keys: [NativeKey] = []
        input.onText = { text, _ in texts.append(text) }
        input.onKey = { keys.append($0) }
        input.setMarkedText("zhongw", selectedRange: NSRange(location: 6, length: 0))
        XCTAssertNotNil(input.markedTextRange)
        XCTAssertFalse(input.hardwareBegan(code: .keyboardReturnOrEnter, characters: "\r", base: "\r", flags: []))
        input.deleteBackward()
        input.textViewDidChange(input)
        XCTAssertTrue(texts.isEmpty); XCTAssertTrue(keys.isEmpty)
        input.setMarkedText("中文", selectedRange: NSRange(location: 2, length: 0))
        input.unmarkText(); input.textViewDidChange(input); input.textViewDidChange(input)
        XCTAssertEqual(texts, ["中文"])
        input.setMarkedText("候选", selectedRange: NSRange(location: 2, length: 0))
        input.admission(ready: false, epoch: 1)
        input.setMarkedText("暂停期间", selectedRange: NSRange(location: 4, length: 0))
        input.admission(ready: true, epoch: 1)
        input.unmarkText(); input.textViewDidChange(input)
        XCTAssertEqual(texts, ["中文"])
        XCTAssertTrue(keys.isEmpty)
    }

    func testHiddenSoftwareKeyboardRetainsFocusAndYieldsToOtherControls() async {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 400, height: 800))
        let root = UIViewController(); window.rootViewController = root
        let view = TerminalContainer(); root.view.addSubview(view)
        view.frame = root.view.bounds
        window.makeKeyAndVisible()
        defer { view.setKeyboard(false, active: false); window.isHidden = true }
        view.setKeyboard(false)
        XCTAssertTrue(view.input.isFirstResponder)
        XCTAssertNotNil(view.input.inputView)
        view.setKeyboard(true)
        XCTAssertTrue(view.input.isFirstResponder)
        XCTAssertNil(view.input.inputView)
        view.setKeyboard(false)
        XCTAssertTrue(view.input.isFirstResponder)
        XCTAssertNotNil(view.input.inputView)
        view.setKeyboard(false, active: false)
        let editor = UITextField(); root.view.addSubview(editor); editor.becomeFirstResponder()
        view.setKeyboard(false, active: false)
        XCTAssertTrue(editor.isFirstResponder, "session forms must keep keyboard focus")
        XCTAssertFalse(view.input.isFirstResponder)
        editor.resignFirstResponder()
    }
}
