import XCTest
import Security
@testable import Voytty

@MainActor
final class SessionWorkflowTests: XCTestCase {
    private let host = SavedHost(NativeHost(deviceId: String(repeating: "a", count: 64), name: "Fixture", relayUrls: []))
    private func session(_ id: String, occupied: Bool = false) -> NativeSession {
        NativeSession(sessionId: id, name: id, occupied: occupied, rows: 24, columns: 80)
    }

    func testHostEntryPolicyUsesRememberedSessionAndRequiresPickerForOccupancy() {
        XCTAssertEqual(HostEntry.resolve([], remembered: nil), .defaultSession)
        XCTAssertEqual(HostEntry.resolve([session("one")], remembered: nil), .session("one"))
        XCTAssertEqual(HostEntry.resolve([session("one", occupied: true)], remembered: nil), .picker)
        XCTAssertEqual(HostEntry.resolve([session("one"), session("two")], remembered: nil), .picker)
        XCTAssertEqual(HostEntry.resolve([session("one", occupied: true), session("two")], remembered: "one"), .session("one"))
        XCTAssertEqual(HostEntry.resolve([session("two")], remembered: "ended"), .session("two"))
    }

    func testOldIdentityMigratesWithoutLosingHostsOrSeed() throws {
        let seed = Data(repeating: 9, count: 32)
        let old: [String: Any] = ["seed": seed.base64EncodedString(), "hosts": [
            ["deviceId": host.id, "name": host.name, "relayUrls": host.relayUrls]
        ]]
        let state = try JSONDecoder().decode(SavedState.self, from: JSONSerialization.data(withJSONObject: old))
        XCTAssertEqual(state.seed, seed)
        XCTAssertEqual(state.hosts.first?.id, host.id)
        XCTAssertNil(state.hosts.first?.lastSession)
        XCTAssertNil(state.activeTerminal)
        XCTAssertNil(state.recent)
        XCTAssertEqual(state.preferences, AppPreferences())
        var invalid = AppPreferences(); invalid.theme = "invalid"; invalid.fontSize = 90
        XCTAssertEqual(invalid.validated, AppPreferences())
    }

    func testRestoreAndRetryNeverCreateOrRetargetAndHomeClearsOnlyActiveRecord() async throws {
        let store = try fixtureStore(active: "old")
        defer { remove(store) }
        let script = SessionScript()
        script.sessions = [session("another")]
        let model = AppModel(store: store, monitorsNetwork: false, makeRuntime: { FakeSessionRuntime(script: script) })
        await model.start()
        XCTAssertEqual(model.route, .terminal)
        XCTAssertEqual(script.connections, ["old"])
        XCTAssertEqual(script.defaults, 0)
        XCTAssertTrue(model.needsTakeover)
        await model.retry()
        XCTAssertEqual(script.connections, ["old", "old"])
        XCTAssertEqual(script.takeovers, [false, false])
        XCTAssertEqual(script.defaults, 0)
        model.goHome()
        XCTAssertEqual(model.route, .home)
        let saved = try store.load()
        XCTAssertNil(saved.activeTerminal)
        XCTAssertEqual(saved.recent, SessionRecord(host: host.id, session: "old"))
        XCTAssertFalse(model.busy)
    }

    func testFailedListCannotAuthorizeDefaultCreationOrForgetRememberedSession() async throws {
        let store = try fixtureStore()
        defer { remove(store) }
        var saved = try store.load(); saved.hosts[0].lastSession = "remembered"; try store.save(saved)
        let script = SessionScript(); script.listFailure = true
        let model = AppModel(store: store, monitorsNetwork: false, makeRuntime: { FakeSessionRuntime(script: script) })
        await model.start()
        await model.select(model.hosts[0])
        XCTAssertEqual(script.defaults, 0)
        XCTAssertTrue(script.connections.isEmpty)
        XCTAssertEqual(try store.load().hosts[0].lastSession, "remembered")
        XCTAssertNotNil(model.error)
        XCTAssertEqual(model.connectionNotice?.kind, .failed)
        model.goHome()
    }

    func testConnectionNoticeDismissalDoesNotRepeatAndCannotClearANewerAttempt() async throws {
        let store = try fixtureStore(active: "exact")
        defer { remove(store) }
        let script = SessionScript(); script.connectionError = "transport_unavailable"
        let model = AppModel(store: store, monitorsNetwork: false, makeRuntime: { FakeSessionRuntime(script: script) })
        await model.start()
        let first = try XCTUnwrap(model.connectionNotice)
        XCTAssertEqual(first.kind, .failed)
        XCTAssertEqual(first.session, "exact")
        model.dismissConnectionNotice(first.id)
        XCTAssertNil(model.connectionNotice)
        await model.refresh()
        await model.networkChanged()
        XCTAssertNil(model.connectionNotice, "metadata/network updates must not repeat a dismissed alert")
        XCTAssertEqual(script.connections, ["exact"], "dismissal never retries or takes over")
        await model.retry()
        let second = try XCTUnwrap(model.connectionNotice)
        XCTAssertNotEqual(first.id, second.id)
        XCTAssertFalse(model.connectionNoticeIsCurrent(first))
        XCTAssertTrue(model.connectionNoticeIsCurrent(second))
        model.dismissConnectionNotice(first.id)
        XCTAssertEqual(model.connectionNotice?.id, second.id)
        model.goHome()
        XCTAssertNil(model.connectionNotice)
        XCTAssertFalse(model.connectionNoticeIsCurrent(second))
    }

    func testLiveConnectionAndAutomaticRecoveryDoNotProduceFailureAlerts() {
        for state in ["active", "synchronizing", "reconnecting"] {
            XCTAssertNil(ConnectionNotice.kind(state: state, code: "transport_unavailable", hasError: true))
        }
        XCTAssertNil(ConnectionNotice.kind(state: nil, code: nil, hasError: false))
        XCTAssertEqual(ConnectionNotice.kind(state: "closed", code: nil, hasError: false), .failed)
    }

    func testRestoreFailureMatrixPreservesIdentityAndNeverCreatesFallback() async throws {
        for code in ["session_not_found", "session_ended", "unauthorized", "transport_unavailable", "controller_busy"] {
            let store = try fixtureStore(active: "exact")
            defer { remove(store) }
            let script = SessionScript(); script.connectionError = code
            let model = AppModel(store: store, monitorsNetwork: false, makeRuntime: { FakeSessionRuntime(script: script) })
            await model.start()
            XCTAssertEqual(model.errorCode, code)
            XCTAssertEqual(model.sessionID, "exact")
            XCTAssertEqual(model.needsTakeover, code == "controller_busy")
            XCTAssertFalse(model.inputReady)
            let expected: ConnectionNotice.Kind = ["session_not_found", "session_ended"].contains(code) ? .ended
                : code == "controller_busy" ? .occupied : .failed
            XCTAssertEqual(model.connectionNotice?.kind, expected)
            XCTAssertEqual(model.connectionNotice?.session, "exact")
            await model.retry()
            XCTAssertEqual(script.connections, ["exact", "exact"])
            XCTAssertEqual(script.defaults, 0)
            model.goHome()
            XCTAssertNil(try store.load().activeTerminal)
        }
    }

    func testLateSessionListCannotReopenHomeOrCreateDefault() async throws {
        let store = try fixtureStore()
        defer { remove(store) }
        let script = SessionScript(); script.holdList = true
        let model = AppModel(store: store, monitorsNetwork: false, makeRuntime: { FakeSessionRuntime(script: script) })
        await model.start()
        let opening = Task { await model.select(model.hosts[0]) }
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while script.waiter == nil {
            guard ContinuousClock.now < deadline else { XCTFail("list never started"); return }
            await Task.yield()
        }
        model.goHome()
        script.waiter?.resume(returning: []); script.waiter = nil
        await opening.value
        XCTAssertEqual(model.route, .home)
        XCTAssertNil(model.selectedHost)
        XCTAssertNil(model.error)
        XCTAssertFalse(model.sessionsRefreshing)
        XCTAssertFalse(model.busy)
        XCTAssertEqual(script.defaults, 0)
        XCTAssertTrue(script.connections.isEmpty)
    }

    func testCreateAttachFailureRetainsExactIdentityAndRenameDeleteDoNotCreate() async throws {
        let store = try fixtureStore()
        defer { remove(store) }
        let script = SessionScript(); script.sessions = [session("one"), session("two")]
        let model = AppModel(store: store, monitorsNetwork: false, makeRuntime: { FakeSessionRuntime(script: script) })
        await model.start()
        await model.select(model.hosts[0])
        XCTAssertTrue(model.panel)
        let created = await model.create("new", directory: "/tmp")
        XCTAssertTrue(created, "dismiss the create form once the host created the session, even if attaching fails")
        XCTAssertEqual(script.createdDirectories, ["/tmp"])
        XCTAssertEqual(model.sessionID, "created")
        XCTAssertEqual(try store.load().hosts[0].lastSession, "created")
        await model.retry()
        XCTAssertEqual(script.createdDirectories.count, 1)
        XCTAssertEqual(script.connections, ["created", "created"])
        let row = try XCTUnwrap(model.sessions.first { $0.sessionId == "created" })
        let renamed = await model.rename(row, name: "renamed")
        XCTAssertTrue(renamed)
        XCTAssertEqual(model.sessionName, "renamed")
        XCTAssertEqual(script.connections.count, 2)
        await model.delete(try XCTUnwrap(model.currentSession))
        XCTAssertFalse(model.sessions.contains { $0.sessionId == "created" })
        XCTAssertNil(try store.load().activeTerminal)
        XCTAssertNil(try store.load().hosts[0].lastSession)
        XCTAssertTrue(model.panel)
        XCTAssertEqual(script.defaults, 0)
        model.goHome()
    }

    func testLateAttachmentIsDetachedAfterCancellingConnection() async throws {
        let store = try fixtureStore()
        defer { remove(store) }
        let script = SessionScript(); script.sessions = [session("one")]; script.holdConnection = true
        let model = AppModel(store: store, monitorsNetwork: false, makeRuntime: { FakeSessionRuntime(script: script) })
        await model.start()
        let opening = Task { await model.select(model.hosts[0]) }
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while script.connectionWaiter == nil {
            guard ContinuousClock.now < deadline else { XCTFail("connection never started"); return }
            await Task.yield()
        }
        model.goHome()
        let terminal = LateTerminal(script: script)
        script.connectionWaiter?.resume(returning: terminal); script.connectionWaiter = nil
        await opening.value
        XCTAssertTrue(script.lateDetached)
        XCTAssertEqual(model.route, .home)
        XCTAssertNil(model.terminal)
        XCTAssertNil(try store.load().activeTerminal)
        XCTAssertNil(try store.load().recent)
    }

    func testSessionEndingBehindLockedKeychainFinishesCleanupOnActivation() async throws {
        var store = try fixtureStore(active: "exact")
        defer { remove(store) }
        let keychain = KeychainAvailability()
        store.update = { keychain.locked ? errSecInteractionNotAllowed : SecItemUpdate($0, $1) }
        let script = SessionScript(); script.sessions = [session("exact")]
        script.terminal = ScriptedTerminal(script: script, id: "exact", frame: TerminalGestureTests.frame())
        let spy = BackgroundTaskSpy()
        let model = AppModel(store: store, monitorsNetwork: false, backgroundConnection: BackgroundConnection(work: spy.work),
                             makeRuntime: { FakeSessionRuntime(script: script) })
        await model.start()
        XCTAssertTrue(model.ownsSession)
        try await waitUntil { script.frameWaiter != nil }
        model.sceneChanged(.background)
        keychain.locked = true
        script.end()
        try await waitUntil { model.frame?.state == "ended" }
        XCTAssertNil(model.error, "a locked Keychain must not turn Session end into a storage failure")
        XCTAssertEqual(model.connectionNotice?.kind, .ended)
        XCTAssertEqual(try store.load().activeTerminal, SessionRecord(host: host.id, session: "exact"))
        keychain.locked = false
        model.sceneChanged(.active)
        XCTAssertNil(try store.load().activeTerminal, "activation finishes the deferred cleanup")
        XCTAssertNil(model.error)
        model.goHome()
    }

    func testDeletingTheAttachedSessionEndsInThePanelWithoutAnEndedAlert() async throws {
        let store = try fixtureStore(active: "exact")
        defer { remove(store) }
        let script = SessionScript(); script.sessions = [session("exact"), session("other")]
        script.terminal = ScriptedTerminal(script: script, id: "exact", frame: TerminalGestureTests.frame())
        let model = AppModel(store: store, monitorsNetwork: false, makeRuntime: { FakeSessionRuntime(script: script) })
        await model.start()
        try await waitUntil { script.frameWaiter != nil }
        var observedDuringDelete: ConnectionNotice.Kind?
        script.afterDelete = {
            // The host ends the attachment before closeSession returns.
            script.end()
            try? await self.waitUntil { model.frame?.state == "ended" }
            XCTAssertTrue(model.busy, "the terminal screen suppresses notices while an operation is busy")
            observedDuringDelete = model.connectionNotice?.kind
        }
        await model.delete(try XCTUnwrap(model.currentSession))
        XCTAssertEqual(observedDuringDelete, .ended)
        XCTAssertNil(model.connectionNotice, "deleting the current Session must not leave an ended alert")
        XCTAssertNil(model.terminal)
        XCTAssertNil(model.error)
        XCTAssertTrue(model.panel)
        XCTAssertNil(try store.load().activeTerminal)
        model.goHome()
    }

    private func waitUntil(_ condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while !condition() {
            guard ContinuousClock.now < deadline else { XCTFail("condition timed out"); throw CancellationError() }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    private func fixtureStore(active: String? = nil) throws -> HostStore {
        let store = HostStore(service: "voytty.workflow.\(UUID().uuidString)")
        var saved = try store.load(); saved.hosts = [host]
        saved.activeTerminal = active.map { SessionRecord(host: host.id, session: $0) }
        saved.recent = saved.activeTerminal
        try store.save(saved)
        return store
    }
    private func remove(_ store: HostStore) {
        SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: store.service] as CFDictionary)
    }
}

@MainActor
private final class SessionScript {
    var connectionError = "session_occupied"
    var sessions: [NativeSession] = []
    var connections: [String] = []
    var takeovers: [Bool] = []
    var defaults = 0
    var createdDirectories: [String?] = []
    var listFailure = false
    var holdList = false
    var holdConnection = false
    var lateDetached = false
    var waiter: CheckedContinuation<[NativeSession], Never>?
    var connectionWaiter: CheckedContinuation<NativeTerminal, Never>?
    var terminal: NativeTerminal?
    var frameWaiter: CheckedContinuation<NativeFrame, Never>?
    var afterDelete: (@MainActor () async -> Void)?

    func nextFrame() async -> NativeFrame { await withCheckedContinuation { frameWaiter = $0 } }
    func end() {
        var frame = TerminalGestureTests.frame(); frame.state = "ended"; frame.generation = 2; frame.inputReady = false
        frameWaiter?.resume(returning: frame); frameWaiter = nil
    }

    func list() async throws -> [NativeSession] {
        if listFailure { throw NativeError.RequestFailed(code: "transport_unavailable") }
        if holdList { return await withCheckedContinuation { waiter = $0 } }
        return sessions
    }
    func connect(_ id: String?, takeover: Bool = false) async throws -> NativeTerminal {
        if let id { connections.append(id) } else { defaults += 1 }
        takeovers.append(takeover)
        if let terminal { return terminal }
        if holdConnection { return await withCheckedContinuation { connectionWaiter = $0 } }
        throw NativeError.RequestFailed(code: connectionError)
    }
    func create(_ name: String, directory: String?) -> NativeSession {
        createdDirectories.append(directory)
        let row = NativeSession(sessionId: "created", name: name, occupied: false, rows: 24, columns: 80)
        sessions.append(row)
        return row
    }
    func rename(_ id: String, name: String) throws -> NativeSession {
        guard let index = sessions.firstIndex(where: { $0.sessionId == id }) else { throw NativeError.Closed }
        sessions[index].name = name
        return sessions[index]
    }
    func delete(_ id: String) async throws -> NativeSession {
        guard let row = sessions.first(where: { $0.sessionId == id }) else { throw NativeError.Closed }
        sessions.removeAll { $0.sessionId == id }
        await afterDelete?()
        return row
    }
}

/// Read by the store's Keychain update on the main actor; the test flips it there too.
private final class KeychainAvailability: @unchecked Sendable { var locked = false }

/// An attached Session whose next frame the script releases.
private final class ScriptedTerminal: NativeTerminal, @unchecked Sendable {
    let script: SessionScript
    let id: String
    let frame: NativeFrame
    init(script: SessionScript, id: String, frame: NativeFrame) {
        self.script = script; self.id = id; self.frame = frame
        super.init(noHandle: NoHandle())
    }
    required init(unsafeFromHandle handle: UInt64) { fatalError("test fake") }
    override func sessionId() -> String { id }
    override func currentFrame() -> NativeFrame { frame }
    override func waitForFrame(afterGeneration: UInt64) async throws -> NativeFrame { await script.nextFrame() }
    override func nextNotification() async throws -> NativeNotification { throw NativeError.Closed }
    override func setVisible(visible: Bool) async throws {}
    override func resize(viewport: NativeViewport) async throws {}
    override func setDark(dark: Bool) async throws {}
    override func detach() async throws {}
}

private final class LateTerminal: NativeTerminal, @unchecked Sendable {
    let script: SessionScript
    init(script: SessionScript) { self.script = script; super.init(noHandle: NoHandle()) }
    required init(unsafeFromHandle handle: UInt64) { fatalError("test fake") }
    override func detach() async throws { await MainActor.run { script.lateDetached = true } }
}

private final class FakeSessionRuntime: NativeRuntime, @unchecked Sendable {
    let script: SessionScript
    init(script: SessionScript) { self.script = script; super.init(noHandle: NoHandle()) }
    required init(unsafeFromHandle handle: UInt64) { fatalError("test fake") }
    override func initialize(seed: Data, dnsServers: [NativeDnsServer], hosts: [NativeHost]) async throws -> String { "fixture" }
    override func shutdown() async throws {}
    override func updateDns(servers: [NativeDnsServer]) async throws {}
    override func listSessions(host: String) async throws -> [NativeSession] { try await script.list() }
    override func connectTerminal(host: String, session: String, viewport: NativeViewport, dark: Bool, takeover: Bool) async throws -> NativeTerminal {
        try await script.connect(session, takeover: takeover)
    }
    override func connectDefaultTerminal(host: String, viewport: NativeViewport, dark: Bool) async throws -> NativeTerminal {
        try await script.connect(nil)
    }
    override func createSession(host: String, name: String, directory: String?, viewport: NativeViewport, dark: Bool) async throws -> NativeSession {
        await script.create(name, directory: directory)
    }
    override func renameSession(host: String, session: String, name: String) async throws -> NativeSession { try await script.rename(session, name: name) }
    override func closeSession(host: String, session: String) async throws -> NativeSession { try await script.delete(session) }
}
