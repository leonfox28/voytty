import UserNotifications
import UIKit

enum NotificationPermission { case unknown, denied, allowed }

@MainActor
protocol NotificationPlatform: AnyObject {
    func permission() async -> NotificationPermission
    func authorize() async throws
    func post(_ request: UNNotificationRequest, failed: @escaping @MainActor @Sendable () -> Void)
}

@MainActor
final class SystemNotifications: NSObject, NotificationPlatform, UNUserNotificationCenterDelegate {
    private let center = UNUserNotificationCenter.current()
    override init() { super.init(); center.delegate = self }
    func permission() async -> NotificationPermission {
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .notDetermined: return .unknown
        case .authorized, .provisional, .ephemeral:
            return settings.alertSetting == .enabled || settings.notificationCenterSetting == .enabled ? .allowed : .denied
        default: return .denied
        }
    }
    func authorize() async throws { _ = try await center.requestAuthorization(options: [.alert, .sound]) }
    func post(_ request: UNNotificationRequest, failed: @escaping @MainActor @Sendable () -> Void) {
        center.add(request) { error in if error != nil { Task { @MainActor in failed() } } }
    }
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
        willPresent notification: UNNotification, withCompletionHandler complete: @escaping (UNNotificationPresentationOptions) -> Void) {
        complete([.banner, .list, .sound])
    }
}

/// Application lifetime, independent of terminal views. No permission prompt at launch,
/// event backlog, notification content in diagnostics, or delayed scheduling trigger.
@MainActor
final class TerminalNotifications: ObservableObject {
    @Published private(set) var permission = NotificationPermission.unknown
    @Published private(set) var busy = false
    @Published private(set) var appEnabled = true
    @Published var failed = false
    private let platform: NotificationPlatform
    private var revision = 0
    var onFailure: (() -> Void)?

    init(platform: NotificationPlatform) { self.platform = platform }
    var enabled: Bool { appEnabled && permission == .allowed }

    func setAppEnabled(_ value: Bool) {
        if appEnabled != value { revision += 1; appEnabled = value }
    }
    func refresh() async { permission = await platform.permission() }

    /// Returns false for denial; the caller only persists an enabled preference after authorization.
    func requestEnable() async -> Bool {
        guard !busy else { return false }
        busy = true; failed = false
        defer { busy = false }
        await refresh()
        if permission == .unknown {
            do { try await platform.authorize() }
            catch { failed = true; onFailure?() }
            await refresh()
        }
        return permission == .allowed
    }

    @discardableResult
    func deliver(_ event: NativeNotification, current: () -> Bool) async -> Bool {
        // Events arriving while disabled are consumed, even if authorization changes during an await.
        guard enabled, current() else { return false }
        let admitted = revision
        let fresh = await platform.permission()
        permission = fresh
        guard fresh == .allowed, enabled, admitted == revision, !Task.isCancelled, current() else { return false }
        let content = UNMutableNotificationContent()
        content.title = event.title.flatMap { $0.isEmpty ? nil : $0 } ?? "zterm"
        content.body = event.body
        content.sound = .default
        // Check the repository epoch and native generation immediately before the platform call.
        platform.post(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)) { [weak self] in
            self?.onFailure?()
        }
        return true
    }
}
