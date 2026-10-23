import XCTest
@testable import Zterm

final class TerminalInputQueueTests: XCTestCase {
    @MainActor
    func testSlowWriterPreservesBurstAndEditingOrder() async {
        let started = expectation(description: "first write suspended")
        var release: CheckedContinuation<Void, Never>?
        var sent: [TerminalInputQueue.Input] = []
        let queue = TerminalInputQueue { entry in
            if sent.isEmpty {
                await withCheckedContinuation { release = $0; started.fulfill() }
            }
            sent.append(entry.input)
        }
        XCTAssertTrue(queue.append(.text("printf '", modifiers: 0, paste: false), attachment: 1, epoch: 1))
        await fulfillment(of: [started], timeout: 2)
        let burst = String(repeating: "a", count: 300) + "中文"
        for character in burst {
            XCTAssertTrue(queue.append(.text(String(character), modifiers: 0, paste: false), attachment: 1, epoch: 1))
        }
        XCTAssertTrue(queue.append(.key(.left, modifiers: 0), attachment: 1, epoch: 1))
        XCTAssertTrue(queue.append(.text("c", modifiers: 4, paste: false), attachment: 1, epoch: 1))
        XCTAssertTrue(queue.append(.text("paste", modifiers: 0, paste: true), attachment: 1, epoch: 1))
        XCTAssertTrue(queue.append(.key(.enter, modifiers: 0), attachment: 1, epoch: 1))
        release?.resume()
        await queue.waitUntilDrained()
        XCTAssertEqual(sent, [.text("printf '", modifiers: 0, paste: false),
                              .text(burst, modifiers: 0, paste: false), .key(.left, modifiers: 0),
                              .text("c", modifiers: 4, paste: false),
                              .text("paste", modifiers: 0, paste: true), .key(.enter, modifiers: 0)])
    }

    @MainActor
    func testCancelledAttachmentCannotDrainOldInputIntoNewWorker() async {
        let started = expectation(description: "old attachment write suspended")
        let finished = expectation(description: "old writer finished")
        var release: CheckedContinuation<Void, Never>?
        var sent: [String] = []
        let queue = TerminalInputQueue { entry in
            guard case .text(let text, _, _) = entry.input else { return XCTFail("unexpected key") }
            if text == "in-flight" {
                await withCheckedContinuation { release = $0; started.fulfill() }
                finished.fulfill()
            }
            sent.append(text)
        }
        queue.append(.text("in-flight", modifiers: 0, paste: false), attachment: 1, epoch: 1)
        await fulfillment(of: [started], timeout: 2)
        queue.append(.text("must not replay", modifiers: 0, paste: false), attachment: 1, epoch: 1)
        queue.cancel()
        queue.append(.text("new attachment", modifiers: 0, paste: false), attachment: 2, epoch: 2)
        await queue.waitUntilDrained()
        release?.resume()
        await fulfillment(of: [finished], timeout: 2)
        XCTAssertEqual(sent, ["new attachment", "in-flight"])
        queue.append(.text("still usable", modifiers: 0, paste: false), attachment: 2, epoch: 2)
        await queue.waitUntilDrained()
        XCTAssertEqual(sent.last, "still usable")
    }

    @MainActor
    func testQueueBoundsRejectWholeInputAndResetOnCancel() async {
        var sent: [TerminalInputQueue.Input] = []
        let queue = TerminalInputQueue { sent.append($0.input) }
        let maximum = String(repeating: "x", count: 512 * 1024)
        XCTAssertTrue(queue.append(.text(maximum, modifiers: 0, paste: true), attachment: 1, epoch: 1))
        XCTAssertFalse(queue.append(.text("extra", modifiers: 0, paste: false), attachment: 1, epoch: 1))
        queue.cancel()
        XCTAssertFalse(queue.append(.text(maximum + "x", modifiers: 0, paste: true), attachment: 1, epoch: 1))
        for _ in 0..<256 { XCTAssertTrue(queue.append(.key(.left, modifiers: 0), attachment: 1, epoch: 1)) }
        XCTAssertFalse(queue.append(.key(.enter, modifiers: 0), attachment: 1, epoch: 1))
        queue.cancel()
        XCTAssertTrue(queue.append(.text("accepted", modifiers: 0, paste: false), attachment: 2, epoch: 2))
        await queue.waitUntilDrained()
        XCTAssertEqual(sent, [.text("accepted", modifiers: 0, paste: false)])
    }
}
