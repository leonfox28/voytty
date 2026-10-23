import SwiftUI
import Network

enum AppRoute: Hashable { case home, settings, scanner, terminal }

/// Only explicit host entry can choose another session or create the default.
/// Restoration and occupied-session retries always retain their exact identity.
enum HostEntry: Equatable {
    case session(String), defaultSession, picker
    static func resolve(_ sessions: [NativeSession], remembered: String?) -> Self {
        if let remembered, sessions.contains(where: { $0.sessionId == remembered }) { return .session(remembered) }
        if sessions.isEmpty { return .defaultSession }
        if sessions.count == 1, !sessions[0].occupied { return .session(sessions[0].sessionId) }
        return .picker
    }
}

@MainActor
final class AppModel: ObservableObject {
    @Published var hosts: [SavedHost] = []
    @Published var selectedHost: SavedHost?
    @Published var sessions: [NativeSession] = []
    @Published var terminal: NativeTerminal?
    @Published var frame: NativeFrame?
    @Published var sessionName = ""
    @Published var sessionID: String?
    @Published var route: AppRoute = .home
    @Published var panel = false
    @Published var recent: SessionRecord?
    @Published var preferences = AppPreferences()
    @Published var ready = false
    @Published var busy = false
    @Published var connecting = false
    @Published var sessionsRefreshing = false
    @Published var sessionListError: String?
    @Published var error: String?
    @Published var errorCode: String?
    @Published private(set) var connectionNotice: ConnectionNotice?
    @Published var control = false
    @Published var alt = false
    let uploads = TerminalUploads()
    let notifications: TerminalNotifications
    let diagnostics: AppDiagnostics?
    let backgroundConnection: BackgroundConnection
    var onInputActivity: (() -> Void)?
    private(set) var runtime: NativeRuntime?
    private var saved: SavedState?
    private let store: HostStore
    private let makeRuntime: () throws -> NativeRuntime
    private let monitorsNetwork: Bool
    private var observation: Task<Void, Never>?
    private var notificationObservation: Task<Void, Never>?
    private var closing: Task<Void, Never>?
    private var inputQueue: TerminalInputQueue!
    private var attachmentGeneration = 0
    private var pointerGeometry = 0
    private var resizing: Task<Void, Never>?
    private var pendingViewport: NativeViewport?
    private var operation = 0
    // SwiftUI may clear the presentation binding before running its button action.
    // Keep that action's identity until a newer notice or attachment replaces it.
    private var lastConnectionNoticeID: UUID?
    private var listRequest = 0
    private let monitor = NWPathMonitor()
    private var started = false
    private var restoring = false
    private var dark = true
    private var applicationPhase: ScenePhase = .active
    private var applicationVisible: Bool { applicationPhase == .active }
    private var visibility: Task<Void, Never>?
    private var viewport = NativeViewport(rows: 24, columns: 80)

    init(store: HostStore = HostStore(), monitorsNetwork: Bool = true,
         notifications: TerminalNotifications? = nil, diagnostics: AppDiagnostics? = nil,
         backgroundConnection: BackgroundConnection? = nil,
         makeRuntime: @escaping () throws -> NativeRuntime = { try NativeRuntime() }) {
        self.store = store
        self.makeRuntime = makeRuntime
        self.monitorsNetwork = monitorsNetwork
        self.notifications = notifications ?? TerminalNotifications(platform: SystemNotifications())
        self.diagnostics = diagnostics
        self.backgroundConnection = backgroundConnection ?? BackgroundConnection()
        self.notifications.onFailure = { [weak diagnostics] in diagnostics?.record(.notificationFailed) }
        inputQueue = TerminalInputQueue { [weak self] entry in await self?.sendInput(entry) }
        uploads.currentTarget = { [weak self] in
            guard let self, let terminal = self.terminal, let frame = self.frame,
                  self.route == .terminal, frame.state == "active" else { return nil }
            return TerminalUploads.Target(attachment: self.attachmentGeneration, epoch: frame.inputEpoch) {
                try await terminal.reserveUpload(inputEpoch: frame.inputEpoch)
            }
        }
        uploads.inputBoundary = { [weak self] in
            self?.onInputActivity?()
            self?.inputQueue.cancel()
            self?.control = false; self?.alt = false
            self?.objectWillChange.send()
        }
    }

    var strings: AppStrings { AppStrings(language: preferences.language) }
    var inputReady: Bool { applicationVisible && frame?.inputReady == true && !uploads.paused }
    var currentSession: NativeSession? { sessions.first { $0.sessionId == sessionID } }
    var recentHost: SavedHost? { hosts.first { $0.id == recent?.host } }
    var ownsSession: Bool { terminal != nil && ["active", "synchronizing", "reconnecting"].contains(frame?.state ?? "") }
    var needsTakeover: Bool {
        sessionID != nil && (frame?.state == "lease_lost" || ["session_occupied", "controller_busy", "lease_lost"].contains(errorCode ?? frame?.error ?? ""))
    }

    /// Called once when a connection attempt finishes or its observer terminates;
    /// ordinary frame refreshes and network recovery never re-present a notice.
    func showConnectionNotice() {
        guard route == .terminal, let host = selectedHost,
              let kind = ConnectionNotice.kind(state: frame?.state, code: errorCode ?? frame?.error,
                                               hasError: error != nil || frame?.error != nil) else { return }
        if let current = connectionNotice, current.host == host.id, current.session == sessionID, current.kind == kind { return }
        connectionNotice = ConnectionNotice(host: host.id, session: sessionID, kind: kind,
            message: error ?? frame?.error.map { message(NativeError.RequestFailed(code: $0)) }
                ?? strings("连接暂时不可用。请检查网络后重试。"))
        lastConnectionNoticeID = connectionNotice?.id
    }

    func dismissConnectionNotice(_ id: UUID) {
        if connectionNotice?.id == id { connectionNotice = nil }
    }

    func connectionNoticeIsCurrent(_ notice: ConnectionNotice) -> Bool {
        route == .terminal && selectedHost?.id == notice.host && sessionID == notice.session
            && lastConnectionNoticeID == notice.id
    }

    func start() async {
        guard !started else { return }
        started = true
        await perform { operationID in
            let state = try self.store.load()
            let native = try self.makeRuntime()
            self.runtime = native
            _ = try await native.initialize(seed: state.seed, dnsServers: PlatformNetwork.servers(), hosts: state.hosts.map(\.native))
            try self.check(operationID)
            self.saved = state
            self.publishSaved()
            self.ready = true
            self.diagnostics?.record(.initialized)
            if self.monitorsNetwork {
                self.monitor.pathUpdateHandler = { [weak self] _ in
                    Task { @MainActor [weak self] in await self?.networkChanged() }
                }
                self.monitor.start(queue: DispatchQueue(label: "zterm.network"))
            }
        }
        guard ready else {
            diagnostics?.record(.initializationFailed)
            try? await runtime?.shutdown()
            runtime = nil; started = false
            return
        }
        await notifications.refresh()
        if let active = saved?.activeTerminal, let host = hosts.first(where: { $0.id == active.host }), route == .home {
            await perform { operationID in
                self.route = .terminal; self.selectedHost = host
                self.sessionID = active.session; self.restoring = true
                try await self.attach(active.session, takeover: false, operationID: operationID)
            }
            await refresh()
        }
    }

    func networkChanged() async {
        guard ready, let runtime else { return }
        diagnostics?.record(.networkChanged)
        do { try await runtime.updateDns(servers: PlatformNetwork.servers()) }
        catch { self.error = self.message(error) }
    }

    @discardableResult
    func pair(_ text: String) async -> Bool {
        await perform { operationID in
            guard let runtime = self.runtime else { throw NativeError.Closed }
            guard text.utf8.count <= runtime.ticketTextLimit() else { throw NativeError.RequestFailed(code: "invalid_ticket") }
            _ = try runtime.inspectTicket(ticket: text)
            let pairing = try await runtime.pairTicket(ticket: text)
            try self.check(operationID)
            var host = SavedHost(pairing.host())
            host.lastSession = self.hosts.first { $0.id == host.id }?.lastSession
            try self.save { state in
                state.hosts.removeAll { $0.id == host.id }
                state.hosts.append(host)
            }
            try runtime.commitPairing(pairing: pairing)
            // Saving/committing the pairing succeeds independently of connecting.
            self.selectedHost = host
        }
    }

    func select(_ host: SavedHost, recent: Bool = false) async {
        await perform { operationID in
            try await self.openHost(host, remembered: recent ? self.recent?.session : host.lastSession, operationID: operationID)
        }
    }

    private func openHost(_ host: SavedHost, remembered: String?, operationID: Int) async throws {
        retire()
        route = .terminal; selectedHost = host; sessions = []
        sessionID = remembered; sessionName = ""; panel = false; restoring = false
        connecting = true
        try save { $0.activeTerminal = nil }
        await closing?.value
        try check(operationID)
        let list = try await fetchSessions()
        try check(operationID)
        if let remembered, !list.contains(where: { $0.sessionId == remembered }) {
            try save { state in
                for i in state.hosts.indices where state.hosts[i].id == host.id { state.hosts[i].lastSession = nil }
                if state.recent == SessionRecord(host: host.id, session: remembered) { state.recent = nil }
            }
            sessionID = nil
        }
        switch HostEntry.resolve(list, remembered: remembered) {
        case .session(let id): try await attach(id, takeover: false, operationID: operationID)
        case .defaultSession:
            try await attach(nil, takeover: false, operationID: operationID)
            await refresh()
        case .picker: panel = true
        }
    }

    func refresh() async {
        guard route == .terminal, selectedHost != nil, !connecting || terminal != nil else { return }
        _ = try? await fetchSessions()
    }

    /// Auxiliary metadata never replaces the attachment or the last good list.
    private func fetchSessions() async throws -> [NativeSession] {
        guard let runtime, let host = selectedHost else { throw NativeError.Closed }
        listRequest += 1
        let request = listRequest
        let generation = attachmentGeneration
        func current() -> Bool {
            request == listRequest && generation == attachmentGeneration && selectedHost?.id == host.id && route == .terminal
        }
        sessionsRefreshing = true; sessionListError = nil
        defer { if current() { sessionsRefreshing = false } }
        do {
            let list = try await runtime.listSessions(host: host.id)
            guard current() else { throw CancellationError() }
            sessions = list
            if let currentSession { sessionName = currentSession.name }
            return list
        } catch {
            guard current() else { throw CancellationError() }
            sessionListError = self.message(error)
            diagnostics?.record(.sessionListFailed, error: error)
            throw error
        }
    }

    func togglePanel() {
        panel.toggle()
        if panel { Task { await refresh() } }
    }

    func forget(_ host: SavedHost) async {
        await perform { _ in
            guard let runtime = self.runtime else { throw NativeError.Closed }
            try self.save { state in
                state.hosts.removeAll { $0.id == host.id }
                if state.recent?.host == host.id { state.recent = nil }
                if state.activeTerminal?.host == host.id { state.activeTerminal = nil }
            }
            try await runtime.forgetHost(host: host.id)
        }
    }

    func connect(_ session: NativeSession, takeover: Bool = false) async {
        await connect(id: session.sessionId, takeover: takeover)
    }

    func connect(id: String, takeover: Bool = false) async {
        if id == sessionID, ownsSession, !takeover { panel = false; return }
        await perform { operationID in
            self.restoring = false
            try await self.attach(id, takeover: takeover, operationID: operationID)
        }
    }

    func retry() async {
        await perform { operationID in
            if self.restoring, let id = self.sessionID {
                try await self.attach(id, takeover: false, operationID: operationID)
            } else if let host = self.selectedHost {
                try await self.openHost(host, remembered: self.sessionID, operationID: operationID)
            }
        }
    }

    @discardableResult
    func create(_ name: String, directory: String = "") async -> Bool {
        var created = false
        await perform { operationID in
            guard let runtime = self.runtime, let host = self.selectedHost else { throw NativeError.Closed }
            let session = try await runtime.createSession(host: host.id, name: name,
                directory: directory.isEmpty ? nil : directory, viewport: self.viewport, dark: self.dark)
            created = true
            try self.check(operationID)
            self.sessions.append(session)
            // Preserve the exact created identity before attach; a retry never repeats create.
            try self.save { state in
                for i in state.hosts.indices where state.hosts[i].id == host.id { state.hosts[i].lastSession = session.sessionId }
                state.activeTerminal = SessionRecord(host: host.id, session: session.sessionId)
            }
            self.restoring = false
            try await self.attach(session.sessionId, takeover: false, operationID: operationID)
            await self.refresh()
        }
        return created
    }

    @discardableResult
    func rename(_ session: NativeSession, name: String) async -> Bool {
        await perform { operationID in
            guard let runtime = self.runtime, let host = self.selectedHost else { throw NativeError.Closed }
            let renamed = try await runtime.renameSession(host: host.id, session: session.sessionId, name: name)
            try self.check(operationID)
            self.sessions = self.sessions.map { $0.sessionId == renamed.sessionId ? renamed : $0 }
            if self.sessionID == renamed.sessionId { self.sessionName = renamed.name }
            await self.refresh()
        }
    }

    func delete(_ session: NativeSession) async {
        await perform { operationID in
            guard let runtime = self.runtime, let host = self.selectedHost else { throw NativeError.Closed }
            _ = try await runtime.closeSession(host: host.id, session: session.sessionId)
            try self.check(operationID)
            self.sessions.removeAll { $0.sessionId == session.sessionId }
            if self.sessionID == session.sessionId { self.retire(); self.sessionID = nil; self.sessionName = "" }
            try self.save { state in
                let record = SessionRecord(host: host.id, session: session.sessionId)
                if state.activeTerminal == record { state.activeTerminal = nil }
                if state.recent == record { state.recent = nil }
                for i in state.hosts.indices where state.hosts[i].id == host.id && state.hosts[i].lastSession == session.sessionId {
                    state.hosts[i].lastSession = nil
                }
            }
            self.panel = true
            await self.refresh()
        }
    }

    private func attach(_ id: String?, takeover: Bool, operationID: Int) async throws {
        guard let host = selectedHost, let runtime else { throw NativeError.Closed }
        retire()
        sessionID = id; sessionName = sessions.first { $0.sessionId == id }?.name ?? ""
        panel = false; connecting = true
        try save { $0.activeTerminal = id.map { SessionRecord(host: host.id, session: $0) } }
        await closing?.value
        try check(operationID)
        let next: NativeTerminal
        if let id {
            next = try await runtime.connectTerminal(host: host.id, session: id, viewport: viewport, dark: dark, takeover: takeover)
        } else {
            next = try await runtime.connectDefaultTerminal(host: host.id, viewport: viewport, dark: dark)
        }
        guard operation == operationID, route == .terminal, selectedHost?.id == host.id else {
            try? await next.detach()
            throw CancellationError()
        }
        let actual = next.sessionId()
        do {
            try save { state in
                for i in state.hosts.indices where state.hosts[i].id == host.id { state.hosts[i].lastSession = actual }
                let record = SessionRecord(host: host.id, session: actual)
                state.recent = record; state.activeTerminal = record
            }
        } catch {
            try? await next.detach()
            throw error
        }
        terminal = next; sessionID = actual
        updateVisibility(next)
        sessionName = sessions.first { $0.sessionId == actual }?.name ?? "main"
        frame = next.currentFrame()
        updateBackgroundConnection()
        let generation = attachmentGeneration
        notificationObservation = Task { [weak self] in
            do {
                while !Task.isCancelled {
                    let event = try await next.nextNotification()
                    guard let self, !Task.isCancelled else { return }
                    await self.notifications.deliver(event) {
                        self.attachmentGeneration == generation && self.terminal === next &&
                            next.notificationIsCurrent(generation: event.generation)
                    }
                }
            } catch {
                if !Task.isCancelled, let self, self.terminal === next,
                   !["ended", "closed", "lease_lost"].contains(next.currentFrame().state) {
                    self.diagnostics?.record(.notificationFailed, error: error)
                }
            }
        }
        observation = Task { [weak self] in
            var current = next.currentFrame()
            var inputReady: Bool?
            do {
                while !Task.isCancelled {
                    guard let self, self.attachmentGeneration == generation, self.terminal === next else { return }
                    self.frame = current
                    self.updateBackgroundConnection()
                    if inputReady != current.inputReady {
                        self.diagnostics?.record(current.inputReady ? .inputReady : .inputBlocked)
                        inputReady = current.inputReady
                    }
                    self.uploads.authorityChanged()
                    if ["ended", "closed", "lease_lost"].contains(current.state) {
                        self.showConnectionNotice()
                        try self.save { state in
                            if state.activeTerminal == SessionRecord(host: host.id, session: actual) { state.activeTerminal = nil }
                        }
                        return
                    }
                    current = try await next.waitForFrame(afterGeneration: current.generation)
                }
            } catch {
                if !Task.isCancelled, let self, self.terminal === next { self.error = self.message(error) }
            }
        }
        // Layout and appearance may have changed while connection was in flight.
        try await next.resize(viewport: viewport)
        try await next.setDark(dark: dark)
    }

    private func retire() {
        connectionNotice = nil; lastConnectionNoticeID = nil
        attachmentGeneration += 1; listRequest += 1
        invalidatePointerGeometry()
        resizing?.cancel(); resizing = nil; pendingViewport = nil
        sessionsRefreshing = false; sessionListError = nil
        observation?.cancel(); observation = nil
        notificationObservation?.cancel(); notificationObservation = nil
        uploads.cancel()
        inputQueue.cancel()
        let previous = terminal
        terminal = nil; frame = nil; control = false; alt = false
        updateBackgroundConnection()
        if let previous {
            let preceding = closing
            closing = Task {
                await preceding?.value
                try? await previous.detach()
            }
        }
    }

    /// Cancel the UI operation immediately. Late FFI results cannot restore this route.
    func goHome() {
        operation += 1
        busy = false; connecting = false; restoring = false
        retire()
        route = .home; selectedHost = nil; sessionID = nil; sessionName = ""; sessions = []; panel = false
        error = nil; errorCode = nil
        if saved != nil {
            do { try save { $0.activeTerminal = nil } }
            catch { self.error = self.message(error) }
        }
    }

    func setPreferences(_ preferences: AppPreferences) {
        do { try save { $0.preferences = preferences.validated } }
        catch { self.error = self.message(error) }
    }

    func setNotificationsEnabled(_ enabled: Bool) async {
        guard ready, !notifications.busy else { return }
        if enabled, !(await notifications.requestEnable()) { return }
        var next = preferences
        next.notificationsEnabled = enabled
        notifications.setAppEnabled(enabled)
        setPreferences(next)
        notifications.setAppEnabled(preferences.notificationsEnabled)
    }

    func setDark(_ value: Bool) {
        guard dark != value else { return }
        dark = value
        guard let terminal else { return }
        Task {
            do { try await terminal.setDark(dark: value) }
            catch { if self.terminal === terminal { self.error = self.message(error) } }
        }
    }

    func resize(_ size: NativeViewport) {
        guard size != viewport else { return }
        viewport = size
        diagnostics?.record(.viewport, columns: size.columns, rows: size.rows)
        invalidatePointerGeometry()
        guard let terminal else { return }
        pendingViewport = size
        guard resizing == nil else { return }
        let generation = attachmentGeneration
        resizing = Task { [weak self] in
            guard let self else { return }
            defer { if self.attachmentGeneration == generation { self.resizing = nil } }
            while !Task.isCancelled, self.attachmentGeneration == generation, self.terminal === terminal,
                  let pending = self.pendingViewport {
                self.pendingViewport = nil
                do { try await terminal.resize(viewport: pending) }
                catch { if self.terminal === terminal { self.error = self.message(error) } }
            }
        }
    }

    func sceneChanged(_ phase: ScenePhase) {
        applicationPhase = phase
        updateBackgroundConnection()
        let visible = applicationVisible
        diagnostics?.record(visible ? .foreground : .background)
        if visible { Task { await notifications.refresh() } }
        if !visible { invalidatePointerGeometry(); inputQueue.cancel(); control = false; alt = false }
        objectWillChange.send()
        guard let terminal else { return }
        updateVisibility(terminal)
    }

    private func updateBackgroundConnection() {
        backgroundConnection.update(phase: applicationPhase, ownsConnection: ownsSession)
    }

    private func updateVisibility(_ terminal: NativeTerminal) {
        let preceding = visibility, visible = applicationVisible
        visibility = Task {
            await preceding?.value
            try? await terminal.setVisible(visible: visible)
        }
    }

    func text(_ text: String, paste: Bool = false) {
        let modifiers: UInt8 = (control ? 4 : 0) | (alt ? 2 : 0)
        control = false; alt = false
        enqueue(.text(text, modifiers: modifiers, paste: paste))
    }

    func key(_ key: NativeKey) {
        let modifiers: UInt8 = (control ? 4 : 0) | (alt ? 2 : 0)
        control = false; alt = false
        enqueue(.key(key, modifiers: modifiers))
    }

    @discardableResult
    func hardware(_ event: TerminalHardwareEvent) -> Bool {
        guard event.epoch == frame?.inputEpoch, enqueue(.hardware(event)) else { return false }
        if event.kind != 3 { control = false; alt = false }
        return true
    }

    func invalidatePointerGeometry() { pointerGeometry += 1 }

    func pointer(_ pointer: TerminalPointer) {
        guard inputReady, let frame, pointer.stamp.accepts(frame) else { return }
        if !inputQueue.append(.pointer(pointer), attachment: attachmentGeneration, epoch: frame.inputEpoch, geometry: pointerGeometry) {
            error = strings("输入队列已满，此次输入未发送。请等待连接恢复后检查命令。")
        }
    }

    @discardableResult
    private func enqueue(_ input: TerminalInputQueue.Input) -> Bool {
        guard inputReady, terminal != nil, let frame else { return false }
        onInputActivity?()
        if !inputQueue.append(input, attachment: attachmentGeneration, epoch: frame.inputEpoch) {
            error = strings("输入队列已满，此次输入未发送。请等待连接恢复后检查命令。")
            return false
        }
        return true
    }

    private func sendInput(_ entry: TerminalInputQueue.Entry) async {
        guard inputReady, attachmentGeneration == entry.attachment, let terminal,
              frame?.inputReady == true, frame?.inputEpoch == entry.epoch else { return }
        do {
            switch entry.input {
            case .text(let text, let modifiers, let paste):
                try await terminal.commitText(inputEpoch: entry.epoch, text: text, modifiers: modifiers, paste: paste)
            case .key(let key, let modifiers):
                try await terminal.sendKey(inputEpoch: entry.epoch, key: key, modifiers: modifiers, kind: 1, text: "")
            case .hardware(let event):
                try await terminal.sendKey(inputEpoch: entry.epoch, key: event.key, modifiers: event.modifiers, kind: event.kind, text: event.text)
            case .pointer(let pointer):
                guard entry.geometry == pointerGeometry, let frame, pointer.stamp.accepts(frame) else { return }
                try await terminal.sendPointer(source: pointer.source, row: pointer.row, column: pointer.column, wheelLines: pointer.wheel)
            }
        } catch {
            // A child may change modes after the frame we checked. Dropping this
            // old coordinate is expected; it must never be retried on a new grid.
            if case .pointer = entry.input, case NativeError.RequestFailed("input_not_ready") = error { return }
            if attachmentGeneration == entry.attachment { self.error = self.message(error) }
        }
    }

    private func publishSaved() {
        guard let saved else { return }
        hosts = saved.hosts; recent = saved.recent; preferences = saved.preferences.validated
        notifications.setAppEnabled(preferences.notificationsEnabled)
        if let id = selectedHost?.id { selectedHost = hosts.first { $0.id == id } }
    }

    private func save(_ change: (inout SavedState) -> Void) throws {
        guard var state = saved else { throw StoreError.corrupt }
        change(&state)
        do { try store.save(state) }
        catch { diagnostics?.record(.storageFailed); throw error }
        saved = state
        publishSaved()
    }

    private func check(_ operationID: Int) throws {
        if operation != operationID { throw CancellationError() }
    }

    @discardableResult
    private func perform(_ action: (Int) async throws -> Void) async -> Bool {
        guard !busy else { return false }
        operation += 1
        let operationID = operation
        busy = true; error = nil; errorCode = nil
        defer {
            if operation == operationID {
                let connectionAttempt = connecting
                busy = false; connecting = false
                if connectionAttempt { showConnectionNotice() }
            }
        }
        do {
            try await action(operationID)
            try check(operationID)
            return true
        } catch is CancellationError { return false }
        catch {
            if operation == operationID {
                diagnostics?.record(.operationFailed, error: error)
                self.error = self.message(error)
                if case NativeError.RequestFailed(let code) = error { errorCode = code }
                if restoring { panel = true }
            }
            return false
        }
    }

    func message(_ error: Error) -> String {
        if let error = error as? StoreError {
            switch error {
            case .keychain(let status): return strings("无法访问设备钥匙串（%@）。请解锁设备后重试。", String(status))
            case .corrupt: return strings("已保存的设备身份无法读取。为保护现有配对，未生成替代身份。")
            }
        }
        if case NativeError.RequestFailed(let code) = error {
            switch code {
            case "invalid_ticket", "pair_ticket_invalid", "pair_ticket_consumed": return strings("配对码无效，请使用完整且未使用的配对码。")
            case "ticket_expired", "pair_ticket_expired", "pairing_expired": return strings("配对码已过期，请在电脑上重新生成。")
            case "session_occupied", "controller_busy": return strings("会话正在使用，接管前需要确认。")
            case "session_not_found", "session_ended": return strings("会话已结束，请选择其他会话。")
            case "lease_lost": return strings("已在其他设备连接。")
            case "invalid_session_name": return strings("会话名称无效。")
            case "session_name_conflict", "session_already_exists": return strings("名称已存在，请使用其他名称。")
            case "invalid_working_directory", "working_directory_unavailable": return strings("目录不可用，请检查电脑上的路径。")
            case "operation_outcome_unknown": return strings("结果未确认，请刷新会话列表后检查。")
            case "unauthorized", "authorization_revoked", "peer_not_authorized": return strings("请重新配对此主机。")
            case "input_not_ready": return strings("等待连接恢复，此次输入未发送。")
            default: return strings("操作未完成（%@）。请检查电脑和网络后重试。", code)
            }
        }
        return strings("连接暂时不可用。请检查网络后重试。")
    }
}
