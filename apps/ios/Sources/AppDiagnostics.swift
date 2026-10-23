import SwiftUI

@MainActor
final class AppDiagnostics: ObservableObject {
    struct Export: Identifiable { let id = UUID(); let url: URL }
    @Published private(set) var remainingSeconds: UInt64 = 0
    @Published private(set) var busy = false
    @Published var failed = false
    @Published private(set) var export: Export?
    private var dismissingExport: UUID?
    private let files: DiagnosticFiles
    private let cleanup: Task<Void, Never>
    let native: NativeDiagnostics

    init(files supplied: DiagnosticFiles? = nil) {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        files = supplied ?? DiagnosticFiles(directory: support.appendingPathComponent("diagnostics", isDirectory: true))
        native = installDiagnostics(sink: files)
        let files = files
        cleanup = Task.detached(priority: .utility) { files.removeExport() }
        record(.started)
    }
    func record(_ event: AppDiagnostic, error: Error? = nil, columns: UInt16 = 0, rows: UInt16 = 0) {
        var category: String?
        if let error, case NativeError.RequestFailed(let code) = error { category = code }
        native.recordApp(event: event, category: category, columns: columns, rows: rows)
    }
    /// View-owned ticker, cancelled when Settings disappears; recorder expiry is independent.
    func observe() async {
        let files = files
        while !Task.isCancelled {
            remainingSeconds = await Task.detached(priority: .utility) {
                (diagnosticRemainingMs(control: files.control()) + 999) / 1000
            }.value
            do { try await Task.sleep(for: .seconds(1)) } catch { return }
        }
    }
    func setEnabled(_ enabled: Bool) async {
        guard !busy else { return }; busy = true; failed = false
        let files = files
        let result = await Task.detached(priority: .utility) {
            Result { try files.setControl(diagnosticControl(enabled: enabled)) }
        }.value
        failed = (try? result.get()) == nil
        remainingSeconds = await Task.detached(priority: .utility) {
            (diagnosticRemainingMs(control: files.control()) + 999) / 1000
        }.value
        busy = false
    }
    func prepareExport() async {
        guard !busy, export == nil else { return }; busy = true; failed = false
        await cleanup.value
        let files = files, native = native
        let result = await Task.detached(priority: .utility) {
            Result {
                // Flush is bounded; export still reports pending loss if the barrier times out.
                _ = native.flush()
                // Saved detail remains useful after the recording window ends.
                return try files.prepareExport(includeDebug: true, lost: native.pendingLost())
            }
        }.value
        switch result {
        case .success(let url): export = Export(url: url)
        case .failure: failed = true; record(.exportFailed)
        }
        busy = false
    }
    func finishExport(id: UUID, saved: Bool) {
        guard export?.id == id else { return }
        dismissingExport = id
        export = nil
        if saved { record(.exportCompleted) }
        // Keep admission closed through UIKit's dismissal animation. Its late
        // onDismiss must never remove a newly prepared export or dismiss its sheet.
        busy = true
    }
    func exportDismissed(id: UUID) {
        finishExport(id: id, saved: false)
        guard dismissingExport == id else { return }
        dismissingExport = nil
        let files = files
        Task {
            await Task.detached(priority: .utility) { files.removeExport() }.value
            busy = false
        }
    }
}
