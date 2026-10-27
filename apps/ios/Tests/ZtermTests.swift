import XCTest
import Security
@testable import Zterm

final class ZtermTests: XCTestCase {
    func testNativeRuntimeAndTicketValidation() async throws {
        let runtime = try NativeRuntime()
        XCTAssertFalse(try runtime.currentState().version.isEmpty)
        XCTAssertThrowsError(try runtime.inspectTicket(ticket: "not-a-ticket"))
        let operation = try runtime.newOperation()
        operation.cancel()
        do {
            _ = try await runtime.waitForState(afterGeneration: 1, operation: operation)
            XCTFail("cancelled wait must fail")
        } catch NativeError.Cancelled {}
        try await runtime.shutdown()
        XCTAssertThrowsError(try runtime.currentState())
    }

    /// One AppModel owns the attachment, viewport and input; a second iPad window must not share it.
    /// iPhone always reports one scene at runtime, so check the shipped manifest itself.
    func testApplicationDeclaresSingleWindowScene() throws {
        let manifest = try XCTUnwrap(Bundle.main.object(forInfoDictionaryKey: "UIApplicationSceneManifest") as? [String: Any])
        XCTAssertEqual(manifest["UIApplicationSupportsMultipleScenes"] as? Bool, false)
    }

    func testKeychainIdentityAndHostsSurviveReload() throws {
        let service = "zterm.tests.\(UUID().uuidString)"
        defer { removeKeychain(service) }
        let store = HostStore(service: service)
        var state = try store.load()
        XCTAssertEqual(state.seed.count, 32)
        let seed = state.seed
        let host = SavedHost(NativeHost(deviceId: String(repeating: "a", count: 64), name: "test", relayUrls: []))
        state.hosts = [host]
        try store.save(state)
        let restored = try store.load()
        XCTAssertEqual(restored.seed, seed)
        XCTAssertEqual(restored.hosts, [host])
        state.hosts = []
        try store.save(state)
        XCTAssertEqual(try store.load().seed, seed)
        XCTAssertTrue(try store.load().hosts.isEmpty)
    }

    func testCorruptIdentityIsNotReplaced() throws {
        let service = "zterm.tests.\(UUID().uuidString)"
        defer { removeKeychain(service) }
        let store = HostStore(service: service)
        _ = try store.load()
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service]
        XCTAssertEqual(SecItemUpdate(query as CFDictionary,
                                    [kSecValueData as String: Data("broken".utf8)] as CFDictionary), errSecSuccess)
        XCTAssertThrowsError(try store.load())
        XCTAssertThrowsError(try store.load())
    }

    @MainActor
    func testCommittedTextEnterBackspaceAndDisconnectAdmission() {
        let input = TerminalInput()
        var text: [String] = []
        var keys: [NativeKey] = []
        input.onText = { value, _ in text.append(value) }
        input.onKey = { keys.append($0) }
        input.admission(ready: true, epoch: 1)
        input.text = "中文🙂"
        input.textViewDidChange(input)
        input.textViewDidChange(input)
        XCTAssertEqual(text, ["中文🙂"])
        XCTAssertFalse(input.textView(input, shouldChangeTextIn: NSRange(location: 0, length: 0), replacementText: "\n"))
        input.deleteBackward()
        XCTAssertEqual(keys, [.enter, .backspace])
        input.admission(ready: false, epoch: 2)
        input.text = "do not replay"
        input.textViewDidChange(input)
        input.admission(ready: true, epoch: 3)
        input.textViewDidChange(input)
        XCTAssertEqual(text, ["中文🙂"])
    }

    @MainActor
    func testMarkedTextIsNotSentBeforeCommit() {
        let input = TerminalInput()
        var received: [String] = []
        input.onText = { text, _ in received.append(text) }
        input.admission(ready: true, epoch: 1)
        input.setMarkedText("zhong", selectedRange: NSRange(location: 5, length: 0))
        input.textViewDidChange(input)
        XCTAssertTrue(received.isEmpty)
        input.setMarkedText("中", selectedRange: NSRange(location: 1, length: 0))
        input.unmarkText()
        input.textViewDidChange(input)
        input.textViewDidChange(input)
        XCTAssertEqual(received, ["中"])
        input.setMarkedText("cancel", selectedRange: NSRange(location: 6, length: 0))
        input.admission(ready: false, epoch: 2)
        input.admission(ready: true, epoch: 3)
        input.textViewDidChange(input)
        XCTAssertEqual(received, ["中"])
    }

    func testPlatformDNSPreservesValidAddresses() {
        let servers = PlatformNetwork.servers()
        XCTAssertLessThanOrEqual(servers.count, 8)
        for server in servers { XCTAssertTrue([4, 16].contains(server.address.count)) }
    }

    private func removeKeychain(_ service: String) {
        SecItemDelete([kSecClass as String: kSecClassGenericPassword,
                       kSecAttrService as String: service] as CFDictionary)
    }
}
