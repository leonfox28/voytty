import Foundation

/// One ordered writer per attachment. Slow transport must not turn a burst of
/// keystrokes into a truncated command; adjacent plain text can share a write.
@MainActor
final class TerminalInputQueue {
    enum Input: Equatable {
        case text(String, modifiers: UInt8, paste: Bool)
        case key(NativeKey, modifiers: UInt8)
        case hardware(TerminalHardwareEvent)
        case pointer(TerminalPointer)

        var bytes: Int {
            if case .text(let text, _, _) = self { return text.utf8.count }
            if case .hardware(let event) = self { return max(1, event.text.utf8.count) }
            return 1
        }
    }

    struct Entry {
        let attachment: Int
        let epoch: UInt64
        let geometry: Int?
        var input: Input
    }

    private let send: @MainActor (Entry) async -> Void
    private var entries: [Entry] = []
    private var bytes = 0
    private var worker: Task<Void, Never>?
    private var generation = 0

    init(send: @escaping @MainActor (Entry) async -> Void) { self.send = send }

    @discardableResult
    func append(_ input: Input, attachment: Int, epoch: UInt64, geometry: Int? = nil) -> Bool {
        guard input.bytes <= 512 * 1024 - bytes else { return false }
        if let last = entries.last, last.attachment == attachment, last.epoch == epoch,
           case .text(let previous, 0, false) = last.input,
           case .text(let next, 0, false) = input,
           previous.utf8.count + next.utf8.count <= 4096 {
            entries[entries.count - 1].input = .text(previous + next, modifiers: 0, paste: false)
        } else {
            guard entries.count < 256 else { return false }
            entries.append(Entry(attachment: attachment, epoch: epoch, geometry: geometry, input: input))
        }
        bytes += input.bytes
        if worker == nil {
            let current = generation
            worker = Task { [weak self] in
                guard let self else { return }
                defer { if self.generation == current { self.worker = nil } }
                while !Task.isCancelled, self.generation == current, !self.entries.isEmpty {
                    let entry = self.entries.removeFirst()
                    self.bytes -= entry.input.bytes
                    await self.send(entry)
                }
            }
        }
        return true
    }

    func cancel() {
        generation += 1
        worker?.cancel()
        worker = nil
        entries.removeAll()
        bytes = 0
    }

    func waitUntilDrained() async { await worker?.value }
}
