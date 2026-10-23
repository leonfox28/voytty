import UIKit

/// Serializes and coalesces UIKit handle movement against one captured native source.
/// Copy waits for the latest range, and stale completions never overwrite the clipboard.
@MainActor
final class TerminalSelectionController {
    struct Request {
        let first: Endpoint
        let last: Endpoint
    }
    struct Endpoint {
        let source: NativeFrameSource
        let row: UInt16
        let column: UInt16
        let logicalRow: Int64
        func matches(_ other: Endpoint) -> Bool { logicalRow == other.logicalRow && column == other.column }
    }
    let terminal: NativeTerminal
    var onFailure: (() -> Void)?
    var onScrollFailure: (() -> Void)?
    private var desired: Request?
    private var revision = 0
    private var applied = 0
    private var worker: Task<Void, Never>?
    private var appliedRequest: Request?
    private var pendingScroll: (UInt64, Bool)?
    private var scrollRevision = 0

    init(terminal: NativeTerminal) { self.terminal = terminal }

    func select(_ request: Request?) {
        desired = request
        if request == nil { pendingScroll = nil }
        revision += 1
        start()
    }

    func scroll(offset: UInt64, older: Bool) {
        pendingScroll = (offset, older)
        start()
    }

    func cancelScrolling() { pendingScroll = nil; scrollRevision += 1 }

    func settled() async { await worker?.value }

    /// Resolve the captured host hyperlink, never interpret arbitrary terminal
    /// text as an app/file URL. A moved selection invalidates an in-flight result.
    func hyperlink() async -> URL? {
        guard desired != nil else { return nil }
        let version = revision
        await worker?.value
        guard revision == version, desired != nil else { return nil }
        guard let value = try? await terminal.selectionHyperlink(), revision == version, desired != nil else { return nil }
        return Self.webURL(value)
    }

    static func webURL(_ value: String?) -> URL? {
        guard let value, let url = URL(string: value), ["http", "https"].contains(url.scheme?.lowercased() ?? ""),
              let host = url.host, !host.isEmpty else { return nil }
        return url
    }

    private func start() {
        guard worker == nil else { return }
        worker = Task { [weak self] in
            guard let self else { return }
            while applied != revision || pendingScroll != nil {
                if applied == revision, let scroll = pendingScroll {
                    pendingScroll = nil
                    let version = scrollRevision
                    do { try await terminal.scrollTo(offset: scroll.0, older: scroll.1, horizon: 4) }
                    catch { if version == scrollRevision { onScrollFailure?() } }
                    continue
                }
                let version = revision
                let request = desired
                do {
                    if let request {
                        if appliedRequest == nil {
                            try await terminal.beginSelection(source: request.first.source, row: request.first.row, column: request.first.column)
                            appliedRequest = Request(first: request.first, last: request.first)
                        } else if !appliedRequest!.first.matches(request.first) {
                            try await terminal.extendSelection(source: request.first.source, row: request.first.row, column: request.first.column, anchor: true)
                            appliedRequest = Request(first: request.first, last: appliedRequest!.last)
                        }
                        if version == revision {
                            try await terminal.extendSelection(source: request.last.source, row: request.last.row,
                                                               column: request.last.column, anchor: false)
                            appliedRequest = request
                        }
                    } else {
                        try await terminal.clearSelection()
                        appliedRequest = nil
                    }
                } catch {
                    if version == revision, request != nil {
                        desired = nil
                        revision += 1
                        pendingScroll = nil
                        onFailure?()
                    }
                }
                applied = version
            }
            worker = nil
        }
    }

    @discardableResult
    func copy() -> Task<Void, Never>? {
        guard desired != nil else { return nil }
        let version = revision
        return Task { [weak self] in
            guard let self else { return }
            await worker?.value
            guard revision == version, desired != nil else { return }
            do {
                let text = try await terminal.copySelection()
                guard revision == version, desired != nil else { return }
                UIPasteboard.general.string = text
            } catch { if revision == version { onFailure?() } }
        }
    }
}
