import XCTest
import UniformTypeIdentifiers
@testable import Voytty

private final class UploadDouble: NativeUploadProtocol, @unchecked Sendable {
    struct Start { let path: String; let size: UInt64; let ext: String }
    private let lock = NSLock()
    private var state = NativeUploadState(generation: 1, phase: "preparing", acceptedBytes: 0, totalBytes: nil, error: nil)
    private var starts: [Start] = []
    private var cancellations = 0
    var started: [Start] { lock.withLock { starts } }
    var cancelled: Int { lock.withLock { cancellations } }
    func cancel() { lock.withLock { cancellations += 1 } }
    func start(path: String, size: UInt64, extension ext: String) throws {
        lock.withLock {
            starts.append(Start(path: path, size: size, ext: ext))
            state = NativeUploadState(generation: 2, phase: "uploading", acceptedBytes: 0, totalBytes: size, error: nil)
        }
    }
    func currentState() -> NativeUploadState { lock.withLock { state } }
    func waitForProgress(afterGeneration: UInt64) async throws -> NativeUploadState { throw NativeError.Closed }
    func finish(_ phase: String, error: String? = nil) {
        lock.withLock {
            state.generation += 1; state.phase = phase; state.error = error
            if phase == "completed" { state.acceptedBytes = state.totalBytes ?? 0 }
        }
    }
}

@MainActor
final class TerminalUploadTests: XCTestCase {
    func testCompletedUploadKeepsInputPausedUntilNativePickerFinishesDismissing() async throws {
        let uploads = TerminalUploads(), native = UploadDouble(), nextNative = UploadDouble()
        var reservations = 0
        uploads.currentTarget = { .init(attachment: 1, epoch: 8) {
            reservations += 1
            return reservations == 1 ? native : nextNative
        } }
        defer { uploads.cancel() }
        uploads.choose(.file)
        try await wait { uploads.state?.phase == "picking" }
        let id = try XCTUnwrap(uploads.state?.id)
        uploads.pickerPresented(id)
        uploads.selected(id, file: try source())
        try await wait { !native.started.isEmpty }
        native.finish("completed")
        try await wait { uploads.state == nil }
        XCTAssertTrue(uploads.paused, "keyboard focus must not be acquired under a dismissing system picker")
        uploads.choose(.file)
        XCTAssertNil(uploads.state)
        uploads.pickerDidDismiss(id)
        XCTAssertFalse(uploads.paused)
        uploads.choose(.file)
        try await wait { uploads.state?.phase == "picking" }
        let next = try XCTUnwrap(uploads.state?.id)
        uploads.pickerPresented(next)
        uploads.pickerDidDismiss(id)
        XCTAssertEqual(uploads.presentedPicker, next)
        uploads.cancel(); uploads.pickerDidDismiss(next)
    }

    func testBackgroundExpiryCancelsUploadAndOldExpiryCannotCancelRetry() async throws {
        var expirations: [@Sendable () -> Void] = [], ended: [UIBackgroundTaskIdentifier] = []
        let background = BackgroundWork(begin: { expiration in
            expirations.append(expiration); return UIBackgroundTaskIdentifier(rawValue: expirations.count)
        }, end: { ended.append($0) })
        let uploads = TerminalUploads(background: background), first = UploadDouble(), second = UploadDouble()
        var reserves = 0
        uploads.currentTarget = { .init(attachment: 1, epoch: 8) { reserves += 1; return reserves == 1 ? first : second } }
        defer { uploads.cancel() }
        uploads.choose(.file)
        try await wait { uploads.state?.phase == "picking" }
        XCTAssertTrue(expirations.isEmpty, "picking does not hold a background assertion")
        uploads.selected(try XCTUnwrap(uploads.state?.id), file: try source())
        try await wait { !first.started.isEmpty }
        XCTAssertEqual(expirations.count, 1)
        expirations[0]()
        XCTAssertEqual(uploads.state?.error, "upload_background_expired")
        XCTAssertFalse(uploads.paused)
        XCTAssertEqual(first.cancelled, 1)
        XCTAssertEqual(ended.count, 1)
        uploads.retry()
        try await wait { !second.started.isEmpty }
        expirations[0]()
        XCTAssertEqual(second.cancelled, 0)
        second.finish("completed")
        try await wait { uploads.state == nil }
        XCTAssertEqual(ended.count, 2)
    }

    private func source(_ name: String = "bytes.pdf", data: Data = Data((0..<200_013).map { UInt8($0 % 251) })) throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
        let url = folder.appendingPathComponent(name)
        try data.write(to: url)
        addTeardownBlock { try? FileManager.default.removeItem(at: folder) }
        return url
    }

    private func wait(line: UInt = #line, _ condition: () -> Bool) async throws {
        let clock = ContinuousClock(), deadline = ContinuousClock.now.advanced(by: .seconds(3))
        while !condition() {
            guard clock.now < deadline else {
                XCTFail("upload condition timed out", line: line)
                throw NSError(domain: "voytty.upload.test.timeout", code: 1)
            }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    func testStagingPreservesBytesAndOwnsOnlyItsPrivateCopy() throws {
        let input = try source("中文 original.PDF")
        var staged: StagedUpload? = try StagedUpload.copy(input, suggestedName: "safe\nname.PDF", cancellation: UploadCancellation())
        let copied = try XCTUnwrap(staged?.url)
        XCTAssertNotEqual(input, copied)
        XCTAssertEqual(try Data(contentsOf: copied), try Data(contentsOf: input))
        XCTAssertEqual(staged?.size, 200_013)
        XCTAssertEqual(staged?.filename, "safename.PDF")
        XCTAssertEqual(staged?.fileExtension, "PDF")
        staged = nil
        XCTAssertFalse(FileManager.default.fileExists(atPath: copied.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: input.path))
    }

    func testInclusiveLimitEmptyFilesDirectoryAndCancellation() throws {
        XCTAssertEqual(uploadLimitBytes(), 50_000_000)
        let input = try source(data: Data(repeating: 7, count: 65_537))
        let exact = try StagedUpload.copy(input, cancellation: UploadCancellation(), limit: 65_537)
        XCTAssertEqual(exact.size, 65_537)
        XCTAssertThrowsError(try StagedUpload.copy(input, cancellation: UploadCancellation(), limit: 65_536)) {
            guard case NativeError.RequestFailed("upload_too_large") = $0 else { return XCTFail("\($0)") }
        }
        XCTAssertThrowsError(try StagedUpload.copy(input.deletingLastPathComponent(), cancellation: UploadCancellation()))
        let cancellation = UploadCancellation(); cancellation.cancel()
        XCTAssertThrowsError(try StagedUpload.copy(input, cancellation: cancellation)) { XCTAssertTrue($0 is CancellationError) }
        let empty = try source("empty", data: Data())
        XCTAssertEqual(try StagedUpload.copy(empty, cancellation: UploadCancellation()).size, 0)
    }

    func testCancelledReservationAndOldPickerCannotResumeNewAttempt() async throws {
        let uploads = TerminalUploads(), old = UploadDouble(), next = UploadDouble()
        var pending: CheckedContinuation<any NativeUploadProtocol, Error>?
        uploads.currentTarget = { .init(attachment: 1, epoch: 8) {
            try await withCheckedThrowingContinuation { pending = $0 }
        } }
        uploads.choose(.file)
        let oldID = try XCTUnwrap(uploads.state?.id)
        try await wait { pending != nil }
        XCTAssertTrue(uploads.paused)
        uploads.cancel()
        uploads.currentTarget = { .init(attachment: 1, epoch: 8) { next } }
        uploads.choose(.image)
        pending?.resume(returning: old)
        try await wait { uploads.state?.phase == "picking" && old.cancelled == 1 }
        uploads.selected(oldID, file: try source())
        XCTAssertEqual(uploads.state?.picker, .image)
        XCTAssertTrue(next.started.isEmpty)
        uploads.cancel()
    }

    func testDuplicateSelectionRetryReusesBytesAndCompletionCleansCopy() async throws {
        let uploads = TerminalUploads(), first = UploadDouble(), second = UploadDouble()
        var reserves = 0, boundaries = 0
        uploads.currentTarget = { .init(attachment: 1, epoch: 8) { reserves += 1; return reserves == 1 ? first : second } }
        uploads.inputBoundary = { boundaries += 1 }
        let file = try source()
        uploads.choose(.file)
        try await wait { uploads.state?.phase == "picking" }
        let id = try XCTUnwrap(uploads.state?.id)
        uploads.selected(id, file: file); uploads.selected(id, file: file)
        try await wait { first.started.count == 1 }
        let staged = try XCTUnwrap(first.started.first).path
        first.finish("failed", error: "upload_storage_failed")
        try await wait { uploads.state?.phase == "failed" }
        XCTAssertFalse(uploads.paused)
        XCTAssertEqual(uploads.state?.error, "upload_storage_failed")
        try FileManager.default.removeItem(at: file)
        uploads.retry()
        try await wait { second.started.count == 1 }
        XCTAssertEqual(second.started.first?.path, staged, "retry retains the prepared bytes when the picker source expires")
        XCTAssertEqual(second.started.first?.size, 200_013)
        XCTAssertTrue(uploads.paused)
        second.finish("completed")
        try await wait { uploads.state == nil && !FileManager.default.fileExists(atPath: staged) }
        XCTAssertGreaterThanOrEqual(boundaries, 4)
        XCTAssertEqual(reserves, 2)
    }

    func testAuthorityChangeCancelsAndRejectsLateFileResult() async throws {
        let uploads = TerminalUploads(), native = UploadDouble()
        var epoch: UInt64 = 8
        uploads.currentTarget = { .init(attachment: 1, epoch: epoch) { native } }
        uploads.choose(.file)
        try await wait { uploads.state?.phase == "picking" }
        let id = try XCTUnwrap(uploads.state?.id)
        epoch = 9; uploads.authorityChanged()
        uploads.selected(id, file: try source())
        XCTAssertNil(uploads.state)
        XCTAssertEqual(native.cancelled, 1)
        XCTAssertTrue(native.started.isEmpty)
    }

    func testPhotoProviderStagesBeforeItsTemporaryFileExpires() async throws {
        let file = try source("fixture.png")
        let provider = try XCTUnwrap(NSItemProvider(contentsOf: file))
        let uploads = TerminalUploads(), native = UploadDouble()
        uploads.currentTarget = { .init(attachment: 1, epoch: 8) { native } }
        uploads.choose(.image)
        try await wait { uploads.state?.phase == "picking" }
        uploads.selected(try XCTUnwrap(uploads.state?.id), image: provider)
        try await wait { native.started.count == 1 }
        let start = try XCTUnwrap(native.started.first)
        XCTAssertEqual(start.ext, "png")
        XCTAssertEqual(try Data(contentsOf: URL(fileURLWithPath: start.path)), try Data(contentsOf: file))
        uploads.cancel()
        try await wait { !FileManager.default.fileExists(atPath: start.path) }
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.path))
    }

    func testCancelledPickerAndFailedReservationReleaseInput() async throws {
        let uploads = TerminalUploads(), native = UploadDouble()
        var fail = true
        uploads.currentTarget = { .init(attachment: 1, epoch: 8) {
            if fail { throw NativeError.RequestFailed(code: "input_not_ready") }
            return native
        } }
        uploads.choose(.file)
        try await wait { uploads.state?.phase == "failed" }
        XCTAssertFalse(uploads.paused)
        fail = false; uploads.retry()
        try await wait { uploads.state?.phase == "picking" }
        uploads.selected(try XCTUnwrap(uploads.state?.id), file: nil)
        XCTAssertNil(uploads.state)
        XCTAssertEqual(native.cancelled, 1)
    }

    func testInputComposedDuringPauseIsDiscardedOnResume() {
        let input = TerminalInput()
        var sent: [String] = []
        input.onText = { sent.append($0); XCTAssertFalse($1) }
        input.admission(ready: true, epoch: 3)
        input.admission(ready: false, epoch: 3)
        input.setMarkedText("不能重放", selectedRange: NSRange(location: 4, length: 0))
        input.admission(ready: true, epoch: 3)
        input.textViewDidChange(input)
        XCTAssertTrue(sent.isEmpty)
        XCTAssertTrue(input.text.isEmpty)
        input.text = "fresh"; input.textViewDidChange(input)
        XCTAssertEqual(sent, ["fresh"])
    }
}
