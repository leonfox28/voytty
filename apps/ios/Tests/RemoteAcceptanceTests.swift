import XCTest
import UIKit
import Security
import CryptoKit
@testable import Zterm

final class RemoteAcceptanceTests: XCTestCase {
    /// This opt-in test uses an explicitly provisioned disposable host, never the user's daemon.
    func testPairCreateInputResizeAndReattach() async throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "AcceptanceTicket", withExtension: "txt"))
        let ticket = try String(contentsOf: url, encoding: .utf8)
        guard !ticket.isEmpty else { throw XCTSkip("requires --ticket-file from a disposable presentation_fixture") }
        executionTimeAllowance = 90
        let runtime = try NativeRuntime()
        do {
            let seed = Data((0..<32).map { _ in UInt8.random(in: .min ... .max) })
            _ = try await runtime.initialize(seed: seed, dnsServers: PlatformNetwork.servers(), hosts: [])
            let decoded = try await MainActor.run {
                try QRImageReader.codes(in: PairingPreferencesTests.qr(ticket))
            }
            let candidates = try PairingCodes.candidates(decoded, limit: runtime.ticketTextLimit(), inspect: runtime.inspectTicket)
            XCTAssertEqual(candidates.count, 1)
            XCTAssertTrue(candidates.first?.ticket == ticket, "QR decoding preserves the full credential")
            let paired = try await runtime.pairTicket(ticket: XCTUnwrap(candidates.first).ticket)
            let host = paired.host()
            try runtime.commitPairing(pairing: paired)
            let name = "ios-" + UUID().uuidString.prefix(8)
            let size = NativeViewport(rows: 24, columns: 60)
            let session = try await runtime.createSession(host: host.deviceId, name: name, directory: nil, viewport: size, dark: true)
            do {
                let list = try await runtime.listSessions(host: host.deviceId)
                XCTAssertTrue(list.contains { $0.sessionId == session.sessionId })
                let terminal = try await runtime.connectTerminal(host: host.deviceId, session: session.sessionId,
                                                                viewport: size, dark: true, takeover: false)
                _ = try await Self.wait(terminal) { Self.shellReady($0) }
                let suffix = "中文-👩🏽‍💻-e\u{301}-" + UUID().uuidString.prefix(8)
                let marker = "ios-" + suffix
                let frame = terminal.currentFrame()
                try await terminal.commitText(inputEpoch: frame.inputEpoch,
                                              text: "printf '\\n%s%s\\n' 'ios-' '\(suffix)'", modifiers: 0, paste: false)
                try await terminal.sendKey(inputEpoch: frame.inputEpoch, key: .enter, modifiers: 0, kind: 1, text: "")
                let output = try await Self.wait(terminal) { Self.text($0).contains(marker) }
                try await Self.checkNativeSelection(terminal, frame: output, marker: marker)
                try await terminal.commitText(inputEpoch: terminal.currentFrame().inputEpoch,
                    text: "printf '\\n\\033]8;;https://example.com/terminal\\aLINK_%s\\033]8;;\\a\\n' CAPTURE\r", modifiers: 0, paste: false)
                let linked = try await Self.wait(terminal) { Self.text($0).contains("LINK_CAPTURE") && Self.shellReady($0) }
                try await Self.checkNativeSelection(terminal, frame: linked, marker: "LINK_CAPTURE", expectedLink: "https://example.com/terminal")
                let image = await Self.terminalPNG(terminal.currentFrame())
                let attachment = XCTAttachment(data: image, uniformTypeIdentifier: "public.png")
                attachment.name = "iOS terminal with real host output"
                attachment.lifetime = .keepAlways
                add(attachment)
                let resized = NativeViewport(rows: 20, columns: 44)
                try await terminal.resize(viewport: resized)
                _ = try await Self.wait(terminal) { $0.viewport == resized && $0.inputReady }
                try await terminal.detach()
                let detached = try await runtime.listSessions(host: host.deviceId)
                XCTAssertTrue(detached.contains { $0.sessionId == session.sessionId })
                let reopened = try await runtime.connectTerminal(host: host.deviceId, session: session.sessionId,
                                                                viewport: resized, dark: true, takeover: false)
                XCTAssertEqual(reopened.sessionId(), session.sessionId)
                _ = try await Self.wait(reopened) { $0.inputReady && Self.text($0).contains(marker) }
                try await Self.checkCrossPageSelection(reopened)
                try await Self.checkUpload(reopened)
                try await reopened.detach()
                _ = try await runtime.closeSession(host: host.deviceId, session: session.sessionId)
            } catch {
                _ = try? await runtime.closeSession(host: host.deviceId, session: session.sessionId)
                throw error
            }
            try await runtime.shutdown()
            try await Self.checkSessionWorkflow(host: host, seed: seed)
        } catch {
            try? await runtime.shutdown()
            throw error
        }
    }

    @MainActor
    private static func checkUpload(_ terminal: NativeTerminal) async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("upload-acceptance-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: folder) }
        let source = folder.appendingPathComponent("exact 中文.pdf")
        let data = Data((0..<2_000_013).map { UInt8($0 % 251) })
        try data.write(to: source)
        let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        let frame = try await wait(terminal) { shellReady($0) && $0.historyOffset == 0 }
        let command = "stty -echo; printf '\\033[2J\\033[H'; printf 'UPLOAD_%s\\n' READY; IFS= read -r uploaded; actual=$(shasum -a 256 \"$uploaded\"); case \"$actual\" in '\(digest) '*) printf 'UPLOAD_%s\\n' EXACT;; *) printf 'UPLOAD_%s\\n' BAD;; esac; rm -f -- \"$uploaded\"; rmdir -- \"${uploaded%/*}\"; stty echo\r"
        try await terminal.commitText(inputEpoch: frame.inputEpoch, text: command, modifiers: 0, paste: false)
        _ = try await wait(terminal) { text($0).contains("UPLOAD_READY") }
        let cancelled = try await terminal.reserveUpload(inputEpoch: frame.inputEpoch)
        cancelled.cancel()
        let clock = ContinuousClock()
        let cancellationDeadline = clock.now.advanced(by: .seconds(3))
        while cancelled.currentState().phase != "cancelled" {
            guard clock.now < cancellationDeadline else { throw NSError(domain: "zterm.upload.cancel.timeout", code: 1) }
            try await Task.sleep(for: .milliseconds(10))
        }
        let uploads = TerminalUploads()
        uploads.currentTarget = { .init(attachment: 1, epoch: frame.inputEpoch) {
            try await terminal.reserveUpload(inputEpoch: frame.inputEpoch)
        } }
        defer { uploads.cancel() }
        uploads.choose(.file)
        let deadline = clock.now.advanced(by: .seconds(45))
        var selected = false
        while let state = uploads.state {
            if state.phase == "failed" { throw NativeError.RequestFailed(code: state.error ?? "upload_source_invalid") }
            guard clock.now < deadline else { throw NSError(domain: "zterm.upload.acceptance.timeout", code: 1) }
            if state.phase == "picking" && !selected {
                selected = true
                XCTAssertTrue(uploads.paused)
                do {
                    try await terminal.commitText(inputEpoch: frame.inputEpoch, text: "MUST_NOT_REPLAY", modifiers: 0, paste: false)
                    XCTFail("input must be paused before opening the picker")
                } catch NativeError.RequestFailed(let code) { XCTAssertEqual(code, "input_not_ready") }
                uploads.selected(state.id, file: source)
                uploads.selected(state.id, file: source)
            }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertTrue(selected)
        XCTAssertEqual(terminal.currentFrame().inputEpoch, frame.inputEpoch)
        XCTAssertFalse(text(terminal.currentFrame()).contains("UPLOAD_EXACT"), "upload must not send Enter")
        try await terminal.sendKey(inputEpoch: frame.inputEpoch, key: .enter, modifiers: 0, kind: 1, text: "")
        let final = try await wait(terminal) { text($0).contains("UPLOAD_EXACT") || text($0).contains("UPLOAD_BAD") }
        XCTAssertTrue(text(final).contains("UPLOAD_EXACT"), "host SHA-256 must match the original 2,000,013 bytes")
        XCTAssertFalse(text(final).contains("UPLOAD_BAD"))
    }

    private static func text(_ frame: NativeFrame) -> String {
        (frame.source?.presentationRows() ?? frame.rows).map { row in
            row.cells.map { $0.width == 0 ? "" : $0.text.isEmpty ? " " : $0.text }.joined()
        }.joined(separator: "\n")
    }

    @MainActor
    private static func checkSessionWorkflow(host: NativeHost, seed: Data) async throws {
        let store = HostStore(service: "zterm.remote-workflow.\(UUID().uuidString)")
        _ = try store.load()
        defer {
            SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: store.service] as CFDictionary)
        }
        try store.save(SavedState(seed: seed, hosts: [SavedHost(host)]))
        let notificationPlatform = NotificationFixture()
        let background = BackgroundTaskSpy()
        let model = AppModel(store: store, monitorsNetwork: false,
                             notifications: TerminalNotifications(platform: notificationPlatform),
                             backgroundConnection: BackgroundConnection(work: background.work))
        await model.start()
        // Enter the picker directly to avoid selecting any pre-existing fixture shell.
        model.selectedHost = model.hosts.first; model.route = .terminal; model.panel = true
        await model.refresh()
        let before = Set(model.sessions.map(\.sessionId))
        var created: [String] = []
        var restoredModel: AppModel?
        do {
            let prefix = "ios-flow-" + UUID().uuidString.prefix(6)
            let firstCreated = await model.create(prefix + "-one", directory: "/tmp")
            XCTAssertTrue(firstCreated, model.error ?? "create failed")
            let first = try XCTUnwrap(model.sessionID); created.append(first)
            let firstTerminal = try XCTUnwrap(model.terminal)
            _ = try await Self.wait(firstTerminal) { Self.shellReady($0) }
            let epoch = firstTerminal.currentFrame().inputEpoch
            try await firstTerminal.commitText(inputEpoch: epoch, text: "printf '\\nWORKFLOW_DIR=%s\\n' \"$PWD\"", modifiers: 0, paste: false)
            try await firstTerminal.sendKey(inputEpoch: epoch, key: .enter, modifiers: 0, kind: 1, text: "")
            _ = try await Self.wait(firstTerminal) { Self.text($0).contains("WORKFLOW_DIR=/tmp") || Self.text($0).contains("WORKFLOW_DIR=/private/tmp") }
            try await Self.checkGestureWorkflow(model: model, terminal: firstTerminal)
            try await Self.checkKeyboardWorkflow(model: model, terminal: firstTerminal)
            let renamed = await model.rename(try XCTUnwrap(model.currentSession), name: prefix + "-renamed")
            XCTAssertTrue(renamed, model.error ?? "rename failed")
            await model.refresh()
            XCTAssertTrue(model.terminal === firstTerminal, "metadata must not reconnect the terminal")
            XCTAssertEqual(model.sessionName, prefix + "-renamed")
            try await Self.checkNotificationWorkflow(model: model, platform: notificationPlatform)
            try await Self.checkLeaseWorkflow(model: model, background: background)
            let secondCreated = await model.create(prefix + "-two")
            XCTAssertTrue(secondCreated, model.error ?? "create failed")
            let second = try XCTUnwrap(model.sessionID); created.append(second)
            XCTAssertNotEqual(second, first)
            await model.connect(try XCTUnwrap(model.sessions.first { $0.sessionId == first }))
            XCTAssertEqual(model.sessionID, first)
            let active = try store.load()
            XCTAssertEqual(active.activeTerminal, SessionRecord(host: host.deviceId, session: first))
            model.goHome()
            XCTAssertNil(try store.load().activeTerminal)
            XCTAssertEqual(try store.load().recent?.session, first)
            await model.select(try XCTUnwrap(model.recentHost), recent: true)
            XCTAssertEqual(model.sessionID, first)
            XCTAssertNotNil(model.terminal)
            model.goHome()
            try await model.runtime?.shutdown()
            // Recreate the app owner from the last active on-disk record, as after termination.
            try store.save(active)
            let restored = AppModel(store: store, monitorsNetwork: false)
            restoredModel = restored
            await restored.start()
            XCTAssertEqual(restored.sessionID, first)
            XCTAssertNotNil(restored.terminal, restored.error ?? "restore failed")
            await restored.refresh()
            XCTAssertEqual(Set(restored.sessions.map(\.sessionId)).subtracting(before), Set(created))
            for id in created {
                await restored.delete(try XCTUnwrap(restored.sessions.first { $0.sessionId == id }))
            }
            XCTAssertNil(try store.load().activeTerminal)
            XCTAssertNil(try store.load().recent)
            restored.goHome()
            try await restored.runtime?.shutdown()
        } catch {
            restoredModel?.goHome()
            try? await restoredModel?.runtime?.shutdown()
            model.goHome()
            // Cleanup uses a fresh endpoint if the first model was already shut down.
            try? await model.runtime?.shutdown()
            let cleanup = try NativeRuntime()
            _ = try await cleanup.initialize(seed: seed, dnsServers: PlatformNetwork.servers(), hosts: [host])
            for id in created { _ = try? await cleanup.closeSession(host: host.deviceId, session: id) }
            try? await cleanup.shutdown()
            throw error
        }
    }

    @MainActor
    private static func checkLeaseWorkflow(model: AppModel, background: BackgroundTaskSpy) async throws {
        let runtime = try XCTUnwrap(model.runtime), host = try XCTUnwrap(model.selectedHost)
        let id = try XCTUnwrap(model.sessionID), original = try XCTUnwrap(model.terminal)
        let size = original.currentFrame().viewport
        model.sceneChanged(.background)
        XCTAssertTrue(model.backgroundConnection.isActive)
        let other = try await runtime.connectTerminal(host: host.id, session: id, viewport: size, dark: true, takeover: true)
        do {
            _ = try await wait(other) { $0.inputReady }
            _ = try await wait(original) { $0.state == "lease_lost" }
            // Let the production observer consume the lease-loss frame.
            let deadline = ContinuousClock.now.advanced(by: .seconds(5))
            while !model.needsTakeover, ContinuousClock.now < deadline { await Task.yield() }
            XCTAssertTrue(model.needsTakeover)
            XCTAssertEqual(model.connectionNotice?.kind, .leaseLost)
            let lostNotice = try XCTUnwrap(model.connectionNotice)
            model.dismissConnectionNotice(lostNotice.id)
            XCTAssertFalse(model.inputReady)
            XCTAssertFalse(model.backgroundConnection.isActive, "lease loss releases background time")
            model.sceneChanged(.active)
            await model.connect(id: id)
            XCTAssertTrue(model.needsTakeover, model.error ?? "occupied attachment should fail")
            XCTAssertEqual(model.connectionNotice?.kind, .occupied)
            XCTAssertFalse(model.connectionNoticeIsCurrent(lostNotice))
            XCTAssertEqual(model.sessionID, id)
            XCTAssertNil(model.terminal)
            await model.connect(id: id, takeover: true)
            let reclaimed = try XCTUnwrap(model.terminal, model.error ?? "takeover failed")
            _ = try await wait(reclaimed) { shellReady($0) }
            _ = try await wait(other) { $0.state == "lease_lost" }
            XCTAssertEqual(reclaimed.sessionId(), id)
            XCTAssertFalse(model.needsTakeover)
            XCTAssertNil(model.connectionNotice)
            let epoch = reclaimed.currentFrame().inputEpoch
            try await reclaimed.commitText(inputEpoch: epoch,
                text: "sleep 1; printf '\\nBACKGROUND_%s\\n' LIVE\r", modifiers: 0, paste: false)
            model.sceneChanged(.inactive)
            XCTAssertFalse(model.backgroundConnection.isActive)
            model.sceneChanged(.background)
            XCTAssertTrue(model.backgroundConnection.isActive)
            XCTAssertFalse(model.inputReady)
            model.text("MUST_NOT_REPLAY_BACKGROUND")
            let backgroundFrame = try await wait(reclaimed) { text($0).contains("BACKGROUND_LIVE") && shellReady($0) }
            XCTAssertEqual(backgroundFrame.inputEpoch, epoch, "the live attachment must survive a short app switch")
            XCTAssertTrue(model.terminal === reclaimed)
            model.sceneChanged(.active)
            await model.networkChanged()
            XCTAssertFalse(model.backgroundConnection.isActive)
            // Exercise the system-expiration path without waiting for an
            // unpredictable OS deadline. Expiration releases time, not ownership.
            model.sceneChanged(.background)
            let grants = background.expirations.count
            try XCTUnwrap(background.expirations.last)()
            XCTAssertFalse(model.backgroundConnection.isActive)
            model.sceneChanged(.background)
            XCTAssertEqual(background.expirations.count, grants)
            XCTAssertTrue(model.terminal === reclaimed)
            XCTAssertFalse(model.inputReady)
            model.sceneChanged(.active)
            model.text("printf '\\nLIFECYCLE_%s\\n' OK\r")
            let live = try await wait(reclaimed) { text($0).contains("LIFECYCLE_OK") && shellReady($0) }
            XCTAssertEqual(live.inputEpoch, epoch)
            XCTAssertEqual(model.sessionID, id)
            XCTAssertFalse(text(live).contains("MUST_NOT_REPLAY_BACKGROUND"))
            // Lease loss closes the actor after publishing its final frame.
            try? await other.detach()
        } catch {
            try? await other.detach()
            throw error
        }
    }

    @MainActor
    private static func checkNotificationWorkflow(model: AppModel, platform: NotificationFixture) async throws {
        func waitForCount(_ count: Int) async throws {
            let deadline = ContinuousClock.now.advanced(by: .seconds(15))
            while platform.requests.count < count {
                guard ContinuousClock.now < deadline else { throw NSError(domain: "zterm.notification.timeout", code: count) }
                try await Task.sleep(for: .milliseconds(20))
            }
            XCTAssertEqual(platform.requests.count, count)
        }
        let terminal = try XCTUnwrap(model.terminal)
        _ = try await wait(terminal) { shellReady($0) }
        model.text("printf '\\033]9;结果: done\\a\\033]777;notify;标题;body;结果\\033\\\\'\r")
        try await waitForCount(2)
        XCTAssertEqual(platform.requests.map { $0.content.title }, ["zterm", "标题"])
        XCTAssertEqual(platform.requests.map { $0.content.body }, ["结果: done", "body;结果"])
        let host = try XCTUnwrap(model.recentHost), id = model.sessionID
        model.goHome()
        await model.select(host, recent: true)
        XCTAssertEqual(model.sessionID, id)
        _ = try await wait(XCTUnwrap(model.terminal)) { shellReady($0) }
        model.text("printf '\\033]9;fresh\\a'\r")
        try await waitForCount(3)
        XCTAssertEqual(platform.requests.last?.content.body, "fresh", "reattachment must not replay notifications")
    }

    @MainActor
    private static func checkKeyboardWorkflow(model: AppModel, terminal: NativeTerminal) async throws {
        let expected = "\u{3}\u{1b}b\u{1b}[Z\u{1b}[H\u{1b}[F\u{1b}[3~\u{1b}[5~\u{1b}[6~\u{1b}OP\u{1b}[24~\u{1b}[A\u{1b}[A\r\u{7f}中文e\u{301}🙂\u{1b}[200~paste\nline\u{1b}[201~"
        let hex = expected.utf8.map { String(format: "%02x", $0) }.joined()
        let program = """
import os,termios,tty,select,time
old=termios.tcgetattr(0)
def collect():
    data=b''; deadline=time.monotonic()+12
    while not data.endswith(b'~END~') and time.monotonic()<deadline:
        if select.select([0],[],[],0.1)[0]: data+=os.read(0,4096)
    return data[:-5] if data.endswith(b'~END~') else data
try:
    tty.setraw(0)
    os.write(1,b'\\x1b[2J\\x1b[H\\x1b[?1l\\x1b[?2004hKEYS_READY\\r\\n')
    actual=collect()
    os.write(1,b'LEGACY_OK\\r\\n' if actual==bytes.fromhex('\(hex)') else b'LEGACY_BAD:'+actual.hex().encode()+b'\\r\\n')
    os.write(1,b'\\x1b[?2004l\\x1b[>3uKEYS_ENHANCED\\r\\n')
    actual=collect()
    expected=b'\\x1b[99;5u\\x1b[99;5:2u\\x1b[99;5:3u'
    os.write(1,b'ENHANCED_OK\\r\\n' if actual==expected else b'ENHANCED_BAD:'+actual.hex().encode()+b'\\r\\n')
finally:
    os.write(1,b'\\x1b[<u\\x1b[?2004l')
    termios.tcsetattr(0,termios.TCSANOW,old)
    print('KEYS_DONE')
"""
        let encoded = Data(program.utf8).base64EncodedString()
        model.text("python3 -c \"import base64;exec(base64.b64decode('\(encoded)'))\"")
        model.key(.enter)
        func sync(_ marker: String) async throws -> NativeFrame {
            let deadline = ContinuousClock.now.advanced(by: .seconds(15))
            while ContinuousClock.now < deadline {
                if let frame = model.frame, frame.inputReady, text(frame).contains(marker) { return frame }
                try await Task.sleep(for: .milliseconds(10))
            }
            throw NSError(domain: "zterm.ios.keyboard.timeout", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "waiting for \(marker): \(model.error ?? "")"])
        }
        let ready = try await sync("KEYS_READY")
        let input = TerminalInput()
        input.admission(ready: true, epoch: ready.inputEpoch)
        input.onHardware = { model.hardware($0) }
        input.onText = { model.text($0, paste: $1) }
        input.onKey = { model.key($0) }
        func press(_ code: UIKeyboardHIDUsage, _ characters: String = "", base: String = "", flags: UIKeyModifierFlags = [], repeatKey: Bool = false) {
            XCTAssertTrue(input.hardwareBegan(code: code, characters: characters, base: base, flags: flags))
            if repeatKey { XCTAssertTrue(input.hardwareBegan(code: code, characters: characters, base: base, flags: flags)) }
            XCTAssertTrue(input.hardwareEnded(code: code))
        }
        press(.keyboardC, "\u{3}", base: "c", flags: .control)
        press(.keyboardB, "∫", base: "b", flags: .alternate)
        press(.keyboardTab, flags: .shift)
        for code in [UIKeyboardHIDUsage.keyboardHome, .keyboardEnd, .keyboardDeleteForward, .keyboardPageUp,
                     .keyboardPageDown, .keyboardF1, .keyboardF12] { press(code) }
        press(.keyboardUpArrow, repeatKey: true)
        press(.keyboardReturnOrEnter); press(.keyboardDeleteOrBackspace)
        input.setMarkedText("zhongwen", selectedRange: NSRange(location: 8, length: 0))
        input.textViewDidChange(input)
        XCTAssertFalse(input.hardwareBegan(code: .keyboardReturnOrEnter, characters: "\r", base: "\r", flags: []))
        input.setMarkedText("中文e\u{301}🙂", selectedRange: NSRange(location: 6, length: 0))
        input.unmarkText(); input.textViewDidChange(input)
        let clipboard = UIPasteboard.general.items
        UIPasteboard.general.string = "paste\nline"
        input.paste(nil)
        UIPasteboard.general.items = clipboard
        model.text("~END~")
        let enhanced = try await sync("KEYS_ENHANCED")
        XCTAssertTrue(text(enhanced).contains("LEGACY_OK"), text(enhanced))
        input.admission(ready: true, epoch: enhanced.inputEpoch)
        press(.keyboardC, "\u{3}", base: "c", flags: .control, repeatKey: true)
        model.text("~END~")
        let result = try await sync("KEYS_DONE")
        XCTAssertTrue(text(result).contains("ENHANCED_OK"), text(result))
        XCTAssertFalse(text(result).contains("_BAD"))
    }

    @MainActor
    private static func checkGestureWorkflow(model: AppModel, terminal: NativeTerminal) async throws {
        let program = #"""
import os,re,termios,tty
old=termios.tcgetattr(0)
counts=dict(P=0,R=0,U=0,D=0,A=0,B=0,X=0,Y=0)
buffer=b''
def show():
    os.write(1, ('\x1b[H\x1b[2JTOUCH READY\r\n'+' '.join(k+'='+str(v) for k,v in counts.items())+'\r\nNative selection stays available').encode())
try:
    tty.setraw(0)
    os.write(1,b'\x1b[?1049h\x1b[?1000h\x1b[?1006h')
    show()
    done=False
    while not done:
        buffer+=os.read(0,1024)
        while buffer:
            match=re.match(rb'\x1b\[<(\d+);(\d+);(\d+)([Mm])',buffer)
            if match:
                code=int(match[1])
                key='U' if code==64 else 'D' if code==65 else 'R' if match[4]==b'm' else 'P'
                counts['X']=int(match[2]);counts['Y']=int(match[3]);counts[key]+=1
                buffer=buffer[match.end():]
            elif buffer[:3] in (b'\x1b[A',b'\x1b[B',b'\x1bOA',b'\x1bOB'):
                counts['A' if buffer[2:3]==b'A' else 'B']+=1;buffer=buffer[3:]
            elif buffer[:1]==b'\x1b': break
            else:
                char=buffer[:1];buffer=buffer[1:]
                if char==b'm': os.write(1,b'\x1b[?1000l\x1b[?1006l\x1b[?1007h')
                if char in (b'q',b'\x03'): done=True
        show()
finally:
    os.write(1,b'\x1b[?1000l\x1b[?1006l\x1b[?1007l\x1b[?1049l')
    termios.tcsetattr(0,termios.TCSANOW,old)
    print('GESTURES_DONE')
"""#
        model.text("python3 -c \"import base64;exec(base64.b64decode('\(Data(program.utf8).base64EncodedString())'))\"")
        model.key(.enter)
        let view = TerminalContainer()
        view.onPointer = { [weak model] in model?.pointer($0) }
        view.onGeometryInvalidated = { [weak model] in model?.invalidatePointerGeometry() }
        model.onInputActivity = { [weak view] in view?.gestures.cancel() }
        defer { view.invalidateCoordinates(); model.onInputActivity = nil }
        func sync(_ predicate: (NativeFrame) -> Bool, line: UInt = #line) async throws -> NativeFrame {
            let clock = ContinuousClock(), deadline = ContinuousClock.now.advanced(by: .seconds(15))
            while true {
                if let frame = model.frame, predicate(frame) {
                    view.frame.size = CGSize(width: CGFloat(frame.viewport.columns) * view.canvas.cellWidth,
                                             height: CGFloat(frame.viewport.rows) * view.canvas.cellHeight)
                    view.layoutIfNeeded(); view.update(frame, terminal: terminal)
                    return frame
                }
                guard clock.now < deadline else {
                    throw NSError(domain: "zterm.gesture.acceptance", code: Int(line), userInfo: [NSLocalizedDescriptionKey: "Gesture wait at line \(line): \(model.frame.map(Self.text) ?? "no frame")"])
                }
                try await Task.sleep(for: .milliseconds(25))
            }
        }
        let mouse = try await sync { $0.pointerMode == .mouse && text($0).contains("TOUCH READY") }
        let point = CGPoint(x: view.cellSize.width * 3.5, y: view.cellSize.height * 2.5)
        for _ in 0..<2 { view.gestures.touch(point); view.gestures.tap(); view.gestures.touch(nil) }
        _ = try await sync { text($0).contains("P=2 R=2") && text($0).contains("X=4 Y=3") }
        view.gestures.touch(point)
        view.gestures.move(to: view.cellSize.height * 3)
        view.gestures.move(to: view.cellSize.height)
        view.gestures.end(velocity: 4000); view.gestures.touch(nil)
        _ = try await sync { text($0).contains("U=3 D=2") }
        let stale = TerminalPointer(source: try XCTUnwrap(mouse.source), stamp: TerminalGestureStamp(mouse), row: 0, column: 0)
        model.pointer(stale)
        model.invalidatePointerGeometry() // Invalidates a queued tap before its writer runs.
        let resized = NativeViewport(rows: mouse.viewport.rows - 2, columns: mouse.viewport.columns)
        model.resize(resized)
        _ = try await sync { $0.viewport == resized && $0.inputReady }
        model.pointer(stale) // Also reject a captured source after the actual resize.
        view.gestures.touch(point); view.gestures.tap(); view.gestures.touch(nil)
        let tapped = try await sync { text($0).contains("P=3 R=3") }
        try await checkNativeSelection(terminal, frame: tapped, marker: "TOUCH READY")
        _ = try await sync { $0.selection == nil && $0.pointerMode == .mouse }
        model.text("m")
        _ = try await sync { $0.pointerMode == .alternateScroll }
        view.gestures.touch(point); view.gestures.tap(); view.gestures.touch(nil)
        view.gestures.touch(point)
        view.gestures.move(to: view.cellSize.height * 2)
        view.gestures.move(to: -view.cellSize.height)
        view.gestures.end(velocity: 4000); view.gestures.touch(nil)
        let alternate = try await sync { text($0).contains("A=2 B=3") }
        XCTAssertTrue(text(alternate).contains("P=3 R=3"), "alternate-scroll tap and stale coordinates must not click")
        XCTAssertEqual(alternate.historyOffset, 0)
        model.text("q")
        _ = try await sync { $0.pointerMode == .none && $0.activeScreen == .main && text($0).contains("GESTURES_DONE") }
    }

    @MainActor
    private static func checkCrossPageSelection(_ terminal: NativeTerminal) async throws {
        let initial = terminal.currentFrame()
        let filler = Int(initial.viewport.columns) * 2 - 14
        let command = "python3 -c 'for i in range(80): print(\"line-%03d|中🙂é\" % i + \"-\"*\(filler))'"
        try await terminal.commitText(inputEpoch: initial.inputEpoch, text: command, modifiers: 0, paste: false)
        try await terminal.sendKey(inputEpoch: initial.inputEpoch, key: .enter, modifiers: 0, kind: 1, text: "")
        // The last output line can arrive before its trailing bytes and the
        // shell prompt. Freeze the selection only once this fixture is idle;
        // a late prompt must not move the edge-scroll baseline underneath it.
        let frame = try await Self.wait(terminal) { Self.text($0).contains("line-079|") && Self.shellReady($0) }
        let view = TerminalContainer()
        view.frame = CGRect(x: 0, y: 0, width: CGFloat(frame.viewport.columns) * view.canvas.cellWidth,
                            height: CGFloat(frame.viewport.rows) * view.canvas.cellHeight)
        view.layoutIfNeeded()
        view.update(frame, terminal: terminal)
        var failures: [String] = []
        view.onSelectionError = { failures.append("selection") }
        view.onHistoryError = { failures.append($0) }
        let range = view.selection.document.text.range(of: "line-075|")
        XCTAssertNotEqual(range.location, NSNotFound)
        view.selection.selectedTextRange = view.selection.document.textRange(range)
        await view.selectionController?.settled()
        let target = view.canvas.visibleFirstRow - 25
        let handle = view.selection.caretRect(for: view.selection.selectedTextRange!.start)
        view.selection.trackTouch(CGPoint(x: handle.minX, y: handle.minY))
        view.selection.trackTouch(CGPoint(x: 0, y: 0))
        defer { view.selection.stopTracking() }
        // Drive the same frame-to-UIView updates as SwiftUI while the real edge timer runs.
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(15))
        var next = terminal.currentFrame()
        while true {
            view.update(next, terminal: terminal)
            if view.canvas.visibleFirstRow <= target { break }
            let remaining = clock.now.duration(to: deadline)
            guard remaining > .zero else { throw NSError(domain: "zterm.ios.edge-scroll.timeout", code: 1) }
            let generation = next.generation
            do {
                next = try await Self.wait(terminal, timeout: remaining) { $0.generation > generation }
            } catch {
                XCTFail("edge state: selection=\(view.selection.hasSelection), finger=\(String(describing: view.selection.dragPoint)), first=\(view.canvas.visibleFirstRow), target=\(target), errors=\(failures)")
                throw error
            }
        }
        view.selection.stopTracking()
        await view.selectionController?.settled()
        XCTAssertTrue(failures.isEmpty, "\(failures)")
        let expected = try XCTUnwrap(view.selection.text(in: XCTUnwrap(view.selection.selectedTextRange)))
        XCTAssertTrue(expected.contains("line-065|中🙂é" + String(repeating: "-", count: filler)), "soft wraps must join without extra line breaks")
        XCTAssertTrue(expected.hasSuffix("line-075|"))
        await view.selectionController?.copy()?.value
        XCTAssertEqual(UIPasteboard.general.string, expected)
        XCTAssertFalse(view.latestButton.isHidden)
        view.returnToLatest()
        await view.selectionController?.settled()
        let live = try await Self.wait(terminal) { $0.historyOffset == 0 && $0.selection == nil }
        view.update(live, terminal: terminal)
        XCTAssertTrue(view.latestButton.isHidden)
        XCTAssertFalse(view.selection.hasSelection)
        // Drive the production display-link coast against actual cached/history
        // pages, then cancel before starting any keyboard input or new selection.
        XCTAssertTrue(view.gesturesReady)
        view.gestures.touch(CGPoint(x: 20, y: 80))
        view.gestures.move(to: view.cellSize.height * 5)
        view.gestures.end(velocity: view.cellSize.height * 80)
        view.gestures.touch(nil)
        let coasted = try await Self.wait(terminal) { $0.historyOffset > 5 }
        view.update(coasted, terminal: terminal)
        view.gestures.cancel()
        await view.selectionController?.settled()
        view.returnToLatest()
        await view.selectionController?.settled()
        _ = try await Self.wait(terminal) { $0.historyOffset == 0 }
    }

    @MainActor
    private static func checkNativeSelection(_ terminal: NativeTerminal, frame: NativeFrame, marker: String, expectedLink: String? = nil) async throws {
        let canvas = TerminalCanvas()
        canvas.update(frame)
        let document = TerminalTextDocument(rows: canvas.visibleRows, firstRow: canvas.visibleFirstRow)
        let range = document.text.range(of: marker)
        XCTAssertNotEqual(range.location, NSNotFound)
        let endpoints = try XCTUnwrap(document.endpoints(range))
        let source = try XCTUnwrap(frame.source).viewportSource(firstRow: frame.firstRow + Int64(frame.windowOffset - frame.historyOffset))
        let controller = TerminalSelectionController(terminal: terminal)
        let request = TerminalSelectionController.Request(
            first: .init(source: source, row: UInt16(endpoints.0.row - canvas.visibleFirstRow), column: UInt16(endpoints.0.column), logicalRow: endpoints.0.row),
            last: .init(source: source, row: UInt16(endpoints.1.row - canvas.visibleFirstRow), column: UInt16(endpoints.1.column), logicalRow: endpoints.1.row))
        controller.select(request)
        await controller.copy()?.value
        XCTAssertEqual(UIPasteboard.general.string, marker, "Copy must preserve wide, surrogate and combining clusters exactly")
        XCTAssertNotNil(terminal.currentFrame().selection)
        let link = await controller.hyperlink()
        XCTAssertEqual(link?.absoluteString, expectedLink)
        UIPasteboard.general.string = "unchanged-after-cancel"
        controller.select(request)
        let staleCopy = controller.copy()
        controller.select(nil)
        await staleCopy?.value
        let clearedLink = await controller.hyperlink()
        XCTAssertNil(clearedLink)
        XCTAssertEqual(UIPasteboard.general.string, "unchanged-after-cancel")
        try await terminal.clearSelection()
    }

    @MainActor
    private static func terminalPNG(_ frame: NativeFrame) -> Data {
        let canvas = TerminalCanvas()
        canvas.frame = CGRect(x: 0, y: 0, width: CGFloat(frame.viewport.columns) * canvas.cellWidth,
                              height: CGFloat(frame.viewport.rows) * canvas.cellHeight)
        canvas.update(frame)
        return UIGraphicsImageRenderer(bounds: canvas.bounds).image { _ in canvas.draw(canvas.bounds) }.pngData()!
    }

    private static func shellReady(_ frame: NativeFrame) -> Bool {
        // Admission describes the transport, not login-shell initialization. Wait
        // for the fixture account's Bash/Zsh prompt before its first command.
        guard frame.inputReady, frame.cursorVisible else { return false }
        let rows = frame.source?.presentationRows() ?? frame.rows
        let row = Int(frame.windowOffset - frame.historyOffset) + Int(frame.cursorRow)
        guard rows.indices.contains(row) else { return false }
        let prompt = rows[row].cells.prefix(Int(frame.cursorColumn)).map(\.text).joined().trimmingCharacters(in: .whitespaces)
        return ["$", "#", "%"].contains { prompt.hasSuffix($0) }
    }

    private static func wait(_ terminal: NativeTerminal, timeout: Duration = .seconds(15), line: UInt = #line,
                             until predicate: @escaping @Sendable (NativeFrame) -> Bool) async throws -> NativeFrame {
        // Generated UniFFI Swift futures do not observe Task cancellation. Racing
        // waitForFrame against a timeout would leave the task group waiting forever.
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: timeout)
        while true {
            let frame = terminal.currentFrame()
            if predicate(frame) { return frame }
            let remaining = clock.now.duration(to: deadline)
            guard remaining > .zero else {
                throw NSError(domain: "zterm.ios.acceptance.timeout", code: 1, userInfo: [NSLocalizedDescriptionKey:
                    "Wait at line \(line): state=\(frame.state), path=\(frame.connectionPath), history=\(frame.historyOffset), generation=\(frame.generation)"])
            }
            try await Task.sleep(for: min(.milliseconds(25), remaining))
        }
    }
}
