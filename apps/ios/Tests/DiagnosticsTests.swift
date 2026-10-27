import XCTest
import Darwin
@testable import Zterm

final class DiagnosticsTests: XCTestCase {
    private var directory: URL!
    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("diagnostics-test-" + UUID().uuidString)
    }
    override func tearDownWithError() throws { if FileManager.default.fileExists(atPath: directory.path) { try FileManager.default.removeItem(at: directory) } }

    func testRotationSeparatesLanesBoundsOldFilesAndProtectsStorage() throws {
        let files = DiagnosticFiles(directory: directory, limit: 256)
        XCTAssertEqual(diagnosticRemainingMs(control: files.control()), 0)
        _ = try files.export(includeDebug: true, lost: 0)
        XCTAssertFalse(FileManager.default.fileExists(atPath: directory.path), "read-only inspection creates nothing")
        for i in 0..<80 { XCTAssertTrue(files.append(detail: i % 2 == 0, record: "event-\(i)-" + String(repeating: "x", count: 40) + "\n")) }
        let current = directory.appendingPathComponent("events.jsonl")
        let previous = try Data(contentsOf: current)
        for _ in 0..<20 { XCTAssertTrue(files.append(detail: true, record: String(repeating: "y", count: 100) + "\n")) }
        XCTAssertEqual(try Data(contentsOf: current), previous)
        // Simulate an oversized file left by an older version; normalization keeps complete recent lines.
        try Data(String(repeating: "old\n", count: 1000).utf8).write(to: current)
        XCTAssertTrue(files.append(detail: false, record: "last\n"))
        XCTAssertTrue(files.flush())
        for name in ["events.jsonl", "events.jsonl.1", "detail.jsonl", "detail.jsonl.1"] {
            let url = directory.appendingPathComponent(name)
            let data = try Data(contentsOf: url)
            XCTAssertLessThanOrEqual(data.count, 256); XCTAssertEqual(data.last, 10)
            let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
            XCTAssertEqual((attrs[.posixPermissions] as? NSNumber)?.intValue, 0o600)
            #if targetEnvironment(simulator)
            // Foundation on this simulator omits device Data Protection metadata,
            // even after a successful explicit setAttributes call. This is not
            // evidence of at-rest protection; retain the strict phone assertion.
            XCTAssertNil(attrs[.protectionKey])
            #else
            let protection = (attrs[.protectionKey] as? FileProtectionType)?.rawValue ?? attrs[.protectionKey] as? String
            XCTAssertEqual(protection, FileProtectionType.completeUntilFirstUserAuthentication.rawValue)
            #endif
        }
        XCTAssertEqual(try directory.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup, true)
        XCTAssertFalse(files.append(detail: false, record: "unfinished"))
        XCTAssertFalse(files.append(detail: false, record: String(repeating: "z", count: 4096) + "\n"))
    }

    func testUnsafeFilesAreRejectedWithoutChangingTheirTargets() throws {
        let files = DiagnosticFiles(directory: directory)
        try files.setControl(diagnosticControl(enabled: false))
        let outside = directory.appendingPathComponent("outside")
        try Data("unchanged".utf8).write(to: outside)
        let link = directory.appendingPathComponent("events.jsonl")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: outside)
        XCTAssertFalse(files.append(detail: false, record: "safe\n"))
        XCTAssertThrowsError(try files.export(includeDebug: true, lost: 0))
        XCTAssertEqual(try String(contentsOf: outside, encoding: .utf8), "unchanged")
        try FileManager.default.removeItem(at: link)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: outside.path)
        XCTAssertEqual(Darwin.link(outside.path, link.path), 0)
        XCTAssertFalse(files.append(detail: false, record: "safe\n"), "hard-linked files are not private")
        XCTAssertEqual(try String(contentsOf: outside, encoding: .utf8), "unchanged")
    }

    func testProtectionFailurePreventsPayloadWritesAndAtomicReplacement() throws {
        let existing = DiagnosticFiles(directory: directory)
        XCTAssertTrue(existing.append(detail: false, record: "original\n"))
        let control = diagnosticControl(enabled: true)
        try existing.setControl(control)
        let unavailable = DiagnosticFiles(directory: directory, fileProtection: { _ in throw DiagnosticFiles.Failure.storage })
        XCTAssertFalse(unavailable.append(detail: false, record: "must not be persisted\n"))
        XCTAssertEqual(try String(contentsOf: directory.appendingPathComponent("events.jsonl"), encoding: .utf8), "original\n")
        XCTAssertThrowsError(try unavailable.setControl(diagnosticControl(enabled: false)))
        XCTAssertEqual(existing.control(), control)
        XCTAssertThrowsError(try unavailable.prepareExport(includeDebug: true, lost: 0))
        XCTAssertFalse(FileManager.default.fileExists(atPath: directory.appendingPathComponent("zterm-logs.jsonl").path))
    }

    func testControlExpiryMalformedControlAndExportCancellationCleanup() throws {
        let files = DiagnosticFiles(directory: directory)
        let enabled = diagnosticControl(enabled: true)
        try files.setControl(enabled)
        XCTAssertEqual(files.control(), enabled)
        XCTAssertGreaterThan(diagnosticRemainingMs(control: DiagnosticFiles(directory: directory).control()), 0)
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        try files.setControl(DiagnosticControl(schema: 1, startedMs: now - 901_000, deadlineMs: now - 1000))
        XCTAssertEqual(diagnosticRemainingMs(control: files.control()), 0)
        try Data(String(repeating: "x", count: 1025).utf8).write(to: directory.appendingPathComponent("control.json"))
        XCTAssertEqual(diagnosticRemainingMs(control: files.control()), 0)
        let url = try files.prepareExport(includeDebug: false, lost: 4)
        let header = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        XCTAssertEqual(header["export_schema"] as? Int, 1)
        files.removeExport()
        XCTAssertFalse(FileManager.default.fileExists(atPath: url.path))
        files.removeExport()
        let interrupted = directory.appendingPathComponent(".write-control.json")
        try Data("interrupted".utf8).write(to: interrupted)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: interrupted.path)
        try files.setControl(diagnosticControl(enabled: false))
        XCTAssertFalse(FileManager.default.fileExists(atPath: interrupted.path))
        XCTAssertEqual(diagnosticRemainingMs(control: files.control()), 0)
    }

    func testSharedRecorderFlushExportRedactionAndTimedDetail() async throws {
        let files = DiagnosticFiles(directory: directory, limit: 8192)
        let native = installDiagnostics(sink: files)
        native.recordApp(event: .operationFailed, category: "SECRET_INPUT_SENTINEL", columns: 0, rows: 0)
        XCTAssertTrue(native.flush())
        let initial = try String(contentsOf: directory.appendingPathComponent("events.jsonl"), encoding: .utf8)
        XCTAssertTrue(initial.contains("app_operation_failed")); XCTAssertFalse(initial.contains("SECRET_INPUT_SENTINEL"))
        try files.setControl(diagnosticControl(enabled: true))
        try await wait { native.detailEnabled() }
        native.recordApp(event: .viewport, category: nil, columns: 80, rows: 24)
        XCTAssertTrue(native.flush())
        XCTAssertTrue(files.append(detail: false, record: "unknown SECRET_INPUT_SENTINEL\n"))
        let current = directory.appendingPathComponent("events.jsonl")
        let handle = try FileHandle(forWritingTo: current)
        try handle.seekToEnd(); try handle.write(contentsOf: Data("partial SECRET_INPUT_SENTINEL".utf8)); try handle.close()
        let keyOnly = try files.export(includeDebug: false, lost: 3)
        let all = try files.export(includeDebug: true, lost: 3)
        XCTAssertFalse(String(decoding: keyOnly, as: UTF8.self).contains("viewport_changed"))
        let text = String(decoding: all, as: UTF8.self)
        XCTAssertTrue(text.contains("viewport_changed")); XCTAssertFalse(text.contains("SECRET_INPUT_SENTINEL"))
        let lines = text.split(separator: "\n")
        let header = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(lines[0].utf8)) as? [String: Any])
        XCTAssertEqual(header["omitted"] as? Int, 2)
        for line in lines.dropFirst() { XCTAssertNotNil(diagnosticExportRecord(line: Data(line.utf8))) }
        let detail = directory.appendingPathComponent("detail.jsonl")
        let before = try Data(contentsOf: detail)
        try files.setControl(diagnosticControl(enabled: false))
        try await wait { !native.detailEnabled() }
        native.recordApp(event: .viewport, category: nil, columns: 90, rows: 30)
        XCTAssertTrue(native.flush()); XCTAssertEqual(try Data(contentsOf: detail), before)
    }

    @MainActor
    func testExportIncludesSavedDetailAfterRecordingStops() async throws {
        let files = DiagnosticFiles(directory: directory)
        let diagnostics = AppDiagnostics(files: files)
        await diagnostics.setEnabled(true)
        try await wait { diagnostics.native.detailEnabled() }
        diagnostics.record(.viewport, columns: 80, rows: 24)
        XCTAssertTrue(diagnostics.native.flush())
        await diagnostics.setEnabled(false)
        try await wait { !diagnostics.native.detailEnabled() }
        XCTAssertEqual(diagnostics.remainingSeconds, 0)

        await diagnostics.prepareExport()
        let export = try XCTUnwrap(diagnostics.export)
        let text = try String(contentsOf: export.url, encoding: .utf8)
        XCTAssertTrue(text.contains("app_started"), "export includes normal event history")
        XCTAssertTrue(text.contains("viewport_changed"), "stopping recording must not exclude saved detail")
        diagnostics.exportDismissed(id: export.id)
        try await wait { !diagnostics.busy }
        XCTAssertFalse(FileManager.default.fileExists(atPath: export.url.path))
    }

    @MainActor
    func testExportCannotRestartUntilSystemSheetFinishesDismissing() async throws {
        let files = DiagnosticFiles(directory: directory)
        let diagnostics = AppDiagnostics(files: files)
        await diagnostics.prepareExport()
        let first = try XCTUnwrap(diagnostics.export)
        XCTAssertTrue(FileManager.default.fileExists(atPath: first.url.path))
        diagnostics.finishExport(id: first.id, saved: true)
        XCTAssertTrue(diagnostics.busy)
        await diagnostics.prepareExport()
        XCTAssertNil(diagnostics.export, "a late sheet dismissal must not race a second export")
        diagnostics.exportDismissed(id: first.id)
        let deadline = ContinuousClock.now.advanced(by: .seconds(3))
        while diagnostics.busy, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(diagnostics.busy)
        XCTAssertFalse(FileManager.default.fileExists(atPath: first.url.path))
        await diagnostics.prepareExport()
        let second = try XCTUnwrap(diagnostics.export)
        XCTAssertNotEqual(first.id, second.id)
        XCTAssertTrue(FileManager.default.fileExists(atPath: second.url.path))
        diagnostics.finishExport(id: first.id, saved: false)
        diagnostics.exportDismissed(id: first.id)
        await Task.yield()
        XCTAssertEqual(diagnostics.export?.id, second.id, "late UIKit callbacks cannot dismiss a newer picker")
        XCTAssertTrue(FileManager.default.fileExists(atPath: second.url.path))
        diagnostics.finishExport(id: second.id, saved: false)
        diagnostics.exportDismissed(id: second.id)
        while diagnostics.busy, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(diagnostics.busy)
        XCTAssertFalse(FileManager.default.fileExists(atPath: second.url.path))
    }
    /// Runs on the caller's isolation, so a main-actor predicate never crosses actors.
    private func wait(isolation: isolated (any Actor)? = #isolation, _ predicate: () -> Bool) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(3))
        while !predicate() {
            guard ContinuousClock.now < deadline else { XCTFail("recorder did not observe control within its bounded deadline"); return }
            try await Task.sleep(for: .milliseconds(20))
        }
    }
}
