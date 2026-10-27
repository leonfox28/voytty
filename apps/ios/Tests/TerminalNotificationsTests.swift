import XCTest
import UserNotifications
@testable import Voytty

@MainActor
final class NotificationFixture: NotificationPlatform {
    var status: NotificationPermission = .allowed
    var requests: [UNNotificationRequest] = []
    var requested = 0
    var fail = false
    var permissionRead: (() async -> Void)?
    func permission() async -> NotificationPermission {
        await permissionRead?()
        return status
    }
    func authorize() async throws { requested += 1; status = .allowed }
    func post(_ request: UNNotificationRequest, failed: @escaping @MainActor @Sendable () -> Void) {
        if fail { failed() } else { requests.append(request) }
    }
}

@MainActor
final class TerminalNotificationsTests: XCTestCase {
    private let event = NativeNotification(generation: 7, title: nil, body: "结果: done")

    func testPermissionIsOnlyRequestedFromExplicitEnableAndDeniedEventsNeverReplay() async {
        let platform = NotificationFixture(); platform.status = .unknown
        let notifications = TerminalNotifications(platform: platform)
        await notifications.refresh()
        XCTAssertEqual(platform.requested, 0)
        let dropped = await notifications.deliver(event) { true }
        XCTAssertFalse(dropped)
        let enabled = await notifications.requestEnable()
        XCTAssertTrue(enabled); XCTAssertEqual(platform.requested, 1)
        XCTAssertTrue(platform.requests.isEmpty)
        platform.status = .denied
        await notifications.refresh()
        let denied = await notifications.requestEnable()
        XCTAssertFalse(denied); XCTAssertEqual(platform.requested, 1)
        platform.status = .allowed
        await notifications.refresh()
        XCTAssertTrue(platform.requests.isEmpty)
        let posted = await notifications.deliver(event) { true }
        XCTAssertTrue(posted); XCTAssertEqual(platform.requests.count, 1)
    }

    func testDistinctImmediateRequestsPreserveUnicodeAndPostingFailureDoesNotStopDelivery() async {
        let platform = NotificationFixture()
        let service = TerminalNotifications(platform: platform)
        var failures = 0; service.onFailure = { failures += 1 }
        await service.refresh()
        platform.fail = true
        _ = await service.deliver(event) { true }
        platform.fail = false
        for value in [event, event, NativeNotification(generation: 7, title: "标题", body: "body;结果")] {
            _ = await service.deliver(value) { true }
        }
        XCTAssertEqual(failures, 1)
        XCTAssertEqual(Set(platform.requests.map(\.identifier)).count, 3)
        XCTAssertTrue(platform.requests.allSatisfy { $0.trigger == nil })
        XCTAssertEqual(platform.requests[0].content.title, "voytty")
        XCTAssertEqual(platform.requests[0].content.body, "结果: done")
        XCTAssertEqual(platform.requests[2].content.title, "标题")
        XCTAssertEqual(platform.requests[2].content.body, "body;结果")
    }

    func testPermissionAndAttachmentAreRecheckedAfterSuspensionAndOffOnInvalidatesInFlightEvent() async {
        let platform = NotificationFixture()
        let notifications = TerminalNotifications(platform: platform)
        await notifications.refresh()
        var current = true
        platform.permissionRead = { current = false }
        let stale = await notifications.deliver(event) { current }
        XCTAssertFalse(stale)
        current = true
        platform.permissionRead = { notifications.setAppEnabled(false); notifications.setAppEnabled(true) }
        let toggled = await notifications.deliver(event) { current }
        XCTAssertFalse(toggled)
        platform.permissionRead = { platform.status = .denied }
        let denied = await notifications.deliver(event) { true }
        XCTAssertFalse(denied)
        platform.permissionRead = nil; platform.status = .allowed
        await notifications.refresh()
        notifications.setAppEnabled(false)
        let off = await notifications.deliver(event) { true }
        XCTAssertFalse(off)
        notifications.setAppEnabled(true)
        XCTAssertTrue(platform.requests.isEmpty)
        let fresh = await notifications.deliver(event) { true }
        XCTAssertTrue(fresh); XCTAssertEqual(platform.requests.count, 1)
    }

    func testPreferenceMigrationAndPersistence() throws {
        var preferences = try JSONDecoder().decode(AppPreferences.self, from: Data(#"{"theme":"dark","fontSize":18}"#.utf8))
        XCTAssertTrue(preferences.notificationsEnabled)
        preferences.notificationsEnabled = false
        let restored = try JSONDecoder().decode(AppPreferences.self, from: JSONEncoder().encode(preferences))
        XCTAssertEqual(restored, preferences)
    }
}
