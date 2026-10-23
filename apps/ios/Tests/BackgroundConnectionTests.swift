import XCTest
import UIKit
@testable import Zterm

@MainActor
final class BackgroundTaskSpy {
    var grantsTime = true
    var expirations: [@Sendable () -> Void] = []
    var ended: [UIBackgroundTaskIdentifier] = []
    lazy var work = BackgroundWork(begin: { [unowned self] expiration in
        expirations.append(expiration)
        return grantsTime ? UIBackgroundTaskIdentifier(rawValue: expirations.count) : .invalid
    }, end: { [unowned self] in ended.append($0) })
}

@MainActor
final class BackgroundConnectionTests: XCTestCase {
    func testOnlyOwnedBackgroundConnectionRequestsTimeAndReturnReleasesIt() {
        let spy = BackgroundTaskSpy(), connection = BackgroundConnection(work: spy.work)
        connection.update(phase: .background, ownsConnection: false)
        connection.update(phase: .active, ownsConnection: true)
        connection.update(phase: .inactive, ownsConnection: true)
        XCTAssertTrue(spy.expirations.isEmpty, "system prompts do not start a background visit")
        connection.update(phase: .background, ownsConnection: true)
        XCTAssertTrue(connection.isActive)
        connection.update(phase: .background, ownsConnection: true)
        connection.update(phase: .inactive, ownsConnection: true)
        XCTAssertEqual(spy.expirations.count, 1)
        XCTAssertTrue(spy.ended.isEmpty, "keep the grant through the return transition")
        connection.update(phase: .active, ownsConnection: true)
        connection.update(phase: .active, ownsConnection: true)
        XCTAssertFalse(connection.isActive)
        XCTAssertEqual(spy.ended.map(\.rawValue), [1])
    }

    func testExpiryEndsOnceAndCannotRenewUntilAnotherForegroundVisit() {
        let spy = BackgroundTaskSpy(), connection = BackgroundConnection(work: spy.work)
        connection.update(phase: .background, ownsConnection: true)
        spy.expirations[0]()
        spy.expirations[0]()
        XCTAssertFalse(connection.isActive)
        XCTAssertEqual(spy.ended.map(\.rawValue), [1])
        connection.update(phase: .background, ownsConnection: true)
        connection.update(phase: .inactive, ownsConnection: true)
        connection.update(phase: .background, ownsConnection: true)
        XCTAssertEqual(spy.expirations.count, 1, "frame updates and transitions cannot renew expired time")
        connection.update(phase: .active, ownsConnection: true)
        connection.update(phase: .background, ownsConnection: true)
        spy.expirations[0]()
        XCTAssertTrue(connection.isActive, "late expiration cannot end a later visit")
        XCTAssertEqual(spy.ended.map(\.rawValue), [1])
        connection.update(phase: .active, ownsConnection: true)
        XCTAssertEqual(spy.ended.map(\.rawValue), [1, 2])
    }

    func testSystemDenialDoesNotSpinOrEndInvalidIdentifier() {
        let spy = BackgroundTaskSpy(), connection = BackgroundConnection(work: spy.work)
        spy.grantsTime = false
        connection.update(phase: .background, ownsConnection: true)
        connection.update(phase: .background, ownsConnection: true)
        XCTAssertFalse(connection.isActive)
        XCTAssertEqual(spy.expirations.count, 1)
        XCTAssertTrue(spy.ended.isEmpty)
        connection.update(phase: .active, ownsConnection: true)
        spy.grantsTime = true
        connection.update(phase: .background, ownsConnection: true)
        spy.expirations[0]()
        XCTAssertTrue(connection.isActive)
        connection.update(phase: .active, ownsConnection: true)
        XCTAssertEqual(spy.ended.map(\.rawValue), [2])
    }

    func testDetachOrLeaseLossReleasesTimeWithoutRenewingForLateAttachment() {
        let spy = BackgroundTaskSpy(), connection = BackgroundConnection(work: spy.work)
        connection.update(phase: .background, ownsConnection: true)
        connection.update(phase: .background, ownsConnection: false)
        XCTAssertFalse(connection.isActive)
        XCTAssertEqual(spy.ended.map(\.rawValue), [1])
        connection.update(phase: .background, ownsConnection: true)
        XCTAssertEqual(spy.expirations.count, 1)
        connection.update(phase: .active, ownsConnection: true)
    }

    func testConnectionAndUploadReleaseOnlyTheirOwnAssertions() {
        let connectionSpy = BackgroundTaskSpy(), uploadSpy = BackgroundTaskSpy()
        let connection = BackgroundConnection(work: connectionSpy.work)
        var uploadExpired = false
        uploadSpy.work.begin { uploadExpired = true }
        connection.update(phase: .background, ownsConnection: true)
        connectionSpy.expirations[0]()
        XCTAssertTrue(uploadSpy.work.isActive)
        XCTAssertFalse(uploadExpired)
        connection.update(phase: .active, ownsConnection: true)
        connection.update(phase: .background, ownsConnection: true)
        uploadSpy.expirations[0]()
        XCTAssertTrue(uploadExpired)
        XCTAssertTrue(connection.isActive)
        connection.update(phase: .active, ownsConnection: true)
        XCTAssertEqual(connectionSpy.ended.map(\.rawValue), [1, 2])
        XCTAssertEqual(uploadSpy.ended.map(\.rawValue), [1])
    }
}
