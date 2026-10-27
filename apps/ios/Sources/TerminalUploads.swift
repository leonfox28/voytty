import SwiftUI
import UniformTypeIdentifiers

enum UploadPicker { case image, file }

struct UploadUI: Identifiable {
    let id: UUID
    let picker: UploadPicker
    var phase = "reserving"
    var filename = ""
    var totalBytes: UInt64?
    var acceptedBytes: UInt64 = 0
    var bytesPerSecond = 0.0
    var error: String?
    var active: Bool { !["failed", "completed", "cancelled"].contains(phase) }
    var showProgress: Bool { !["reserving", "picking", "completed", "cancelled"].contains(phase) }
    var fraction: Double { totalBytes.map { $0 == 0 ? 1 : min(1, Double(acceptedBytes) / Double($0)) } ?? 0 }
}

/// App-owned operation, independent of sheet/view recreation. Rust owns the
/// upload binding, input pause and exactly-once path insertion without Enter.
@MainActor
final class TerminalUploads: ObservableObject {
    struct Target {
        let attachment: Int
        let epoch: UInt64
        let reserve: () async throws -> any NativeUploadProtocol
    }
    @Published private(set) var state: UploadUI?
    @Published private(set) var presentedPicker: UUID?
    var paused: Bool { state?.active == true || presentedPicker != nil }
    var currentTarget: () -> Target? = { nil }
    var inputBoundary: () -> Void = {}
    private var current: Attempt?
    private let background: BackgroundWork
    private let staging = DispatchQueue(label: "voytty.upload.staging", qos: .userInitiated)

    init(background: BackgroundWork? = nil) { self.background = background ?? BackgroundWork() }

    private func beginBackground(_ attempt: Attempt) {
        background.begin { [weak self, weak attempt] in
            guard let self, let attempt else { return }
            self.fail(attempt, NativeError.RequestFailed(code: "upload_background_expired"))
        }
    }

    @MainActor private final class Attempt {
        let id = UUID()
        let target: Target
        let picker: UploadPicker
        let cancellation = UploadCancellation()
        var native: (any NativeUploadProtocol)?
        var observation: Task<Void, Never>?
        var preparation: Task<Void, Never>?
        var progress: Progress?
        var staged: StagedUpload?
        var source: URL?
        var provider: NSItemProvider?
        init(target: Target, picker: UploadPicker) { self.target = target; self.picker = picker }
    }

    func choose(_ picker: UploadPicker) { begin(picker) }

    private func begin(_ picker: UploadPicker, staged: StagedUpload? = nil, source: URL? = nil, provider: NSItemProvider? = nil) {
        guard !paused, let target = currentTarget() else { return }
        cancel()
        let attempt = Attempt(target: target, picker: picker)
        attempt.staged = staged; attempt.source = source; attempt.provider = provider
        current = attempt
        state = UploadUI(id: attempt.id, picker: picker)
        inputBoundary()
        attempt.preparation = Task { [weak self, weak attempt] in
            guard let self, let attempt else { return }
            do {
                let native = try await target.reserve()
                guard self.owns(attempt) else { native.cancel(); return }
                attempt.native = native
                self.observe(attempt, native: native)
                if let staged { self.start(staged, attempt: attempt) }
                else if let source { self.stage(source, attempt: attempt) }
                else if let provider { self.load(provider, attempt: attempt) }
                else { self.update(attempt) { $0.phase = "picking" } }
            } catch { self.fail(attempt, error) }
        }
    }

    func selected(_ id: UUID, file: URL?) {
        guard let attempt = current, attempt.id == id, owns(attempt), state?.phase == "picking" else { return }
        guard let file else { cancel(); return }
        attempt.source = file
        stage(file, attempt: attempt)
    }

    func selected(_ id: UUID, image: NSItemProvider?) {
        guard let attempt = current, attempt.id == id, owns(attempt), state?.phase == "picking" else { return }
        guard let image else { cancel(); return }
        attempt.provider = image
        load(image, attempt: attempt)
    }

    func pickerPresented(_ id: UUID) { if state?.id == id { presentedPicker = id } }
    func pickerDidDismiss(_ id: UUID) { if presentedPicker == id { presentedPicker = nil } }

    private func stage(_ source: URL, attempt: Attempt) {
        beginBackground(attempt)
        update(attempt) { $0.phase = "preparing"; $0.filename = source.lastPathComponent }
        let cancellation = attempt.cancellation
        staging.async { [weak self, weak attempt] in
            let result = Result { try StagedUpload.copy(source, cancellation: cancellation) }
            Task { @MainActor in
                guard let self, let attempt, self.owns(attempt) else { return }
                self.prepared(result, attempt: attempt)
            }
        }
    }

    private func load(_ provider: NSItemProvider, attempt: Attempt) {
        beginBackground(attempt)
        update(attempt) { $0.phase = "preparing"; $0.filename = provider.suggestedName ?? "" }
        guard let type = provider.registeredTypeIdentifiers.first(where: { UTType($0)?.conforms(to: .image) == true }) else {
            fail(attempt, NativeError.RequestFailed(code: "upload_source_invalid")); return
        }
        let name = provider.suggestedName
        let cancellation = attempt.cancellation
        attempt.progress = provider.loadFileRepresentation(forTypeIdentifier: type) { [weak self, weak attempt] url, _ in
            // The provider deletes its temporary URL when this callback returns.
            let result = Result {
                guard let url else { throw NativeError.RequestFailed(code: "upload_source_invalid") }
                return try StagedUpload.copy(url, suggestedName: name, cancellation: cancellation)
            }
            Task { @MainActor in
                guard let self, let attempt, self.owns(attempt) else { return }
                self.prepared(result, attempt: attempt)
            }
        }
    }

    private func prepared(_ result: Result<StagedUpload, Error>, attempt: Attempt) {
        guard owns(attempt), state?.phase != "failed" else { return }
        do { start(try result.get(), attempt: attempt) }
        catch { fail(attempt, error) }
    }

    private func start(_ staged: StagedUpload, attempt: Attempt) {
        guard owns(attempt) else { return }
        beginBackground(attempt)
        attempt.staged = staged
        update(attempt) { $0.phase = "preparing"; $0.filename = staged.filename; $0.totalBytes = staged.size }
        do {
            guard let native = attempt.native else { throw NativeError.Closed }
            try native.start(path: staged.url.path, size: staged.size, extension: staged.fileExtension)
        } catch { fail(attempt, error) }
    }

    private func observe(_ attempt: Attempt, native: any NativeUploadProtocol) {
        attempt.observation = Task { [weak self, weak attempt] in
            let clock = ContinuousClock()
            var sampled = clock.now
            var bytes: UInt64 = 0
            var speed = 0.0
            while !Task.isCancelled {
                guard let self, let attempt, self.owns(attempt), self.state?.phase != "failed" else { return }
                let next = native.currentState()
                if ["completed", "cancelled"].contains(next.phase) { self.cancel(); return }
                if next.phase == "failed" {
                    self.fail(attempt, NativeError.RequestFailed(code: next.error ?? "upload_source_invalid")); return
                }
                let duration = sampled.duration(to: clock.now)
                if duration >= .milliseconds(250) {
                    let seconds = Double(duration.components.seconds) + Double(duration.components.attoseconds) / 1e18
                    let rate = Double(next.acceptedBytes - min(bytes, next.acceptedBytes)) / seconds
                    speed = bytes == 0 ? rate : speed * 0.65 + rate * 0.35
                    bytes = next.acceptedBytes; sampled = clock.now
                }
                if !(next.phase == "preparing" && ["reserving", "picking"].contains(self.state?.phase ?? "")) {
                    self.update(attempt) {
                        $0.phase = next.phase; $0.acceptedBytes = next.acceptedBytes
                        $0.totalBytes = next.totalBytes ?? $0.totalBytes; $0.bytesPerSecond = speed
                    }
                }
                // Metadata only. UniFFI waiters do not observe Swift cancellation;
                // bounded polling lets view/attachment retirement release promptly.
                do { try await Task.sleep(for: .milliseconds(100)) } catch { return }
            }
        }
    }

    private func owns(_ attempt: Attempt) -> Bool {
        guard current === attempt, let target = currentTarget() else { return false }
        return target.attachment == attempt.target.attachment && target.epoch == attempt.target.epoch
    }

    private func update(_ attempt: Attempt, _ change: (inout UploadUI) -> Void) {
        guard current === attempt, var value = state else { return }
        change(&value); state = value
    }

    private func fail(_ attempt: Attempt, _ error: Error) {
        guard owns(attempt) else { return }
        background.end()
        attempt.native?.cancel(); attempt.progress?.cancel(); attempt.cancellation.cancel()
        attempt.observation?.cancel()
        let code: String
        if case NativeError.RequestFailed(let value) = error { code = value } else { code = "upload_source_invalid" }
        update(attempt) { $0.phase = "failed"; $0.error = code }
        inputBoundary()
    }

    func authorityChanged() { if let current, !owns(current) { cancel() } }

    func retry() {
        guard let current, !paused else { return }
        begin(current.picker, staged: current.staged, source: current.source, provider: current.provider)
    }

    func cancel() {
        background.end()
        let previous = current
        current = nil; state = nil
        previous?.native?.cancel(); previous?.cancellation.cancel(); previous?.progress?.cancel()
        previous?.preparation?.cancel(); previous?.observation?.cancel()
        if previous != nil { inputBoundary() }
    }
}
