import UIKit

/// A finite UIKit assertion for ongoing user-initiated work.
/// iOS chooses the deadline. Expiry ends the assertion before cancelling work.
@MainActor
final class BackgroundWork {
    typealias Begin = (@escaping @Sendable () -> Void) -> UIBackgroundTaskIdentifier
    private let beginTask: Begin
    private let endTask: (UIBackgroundTaskIdentifier) -> Void
    private var identifier: UIBackgroundTaskIdentifier = .invalid
    private var generation = 0
    var isActive: Bool { identifier != .invalid }

    init(begin: @escaping Begin = { UIApplication.shared.beginBackgroundTask(withName: "Terminal upload", expirationHandler: $0) },
         end: @escaping (UIBackgroundTaskIdentifier) -> Void = { UIApplication.shared.endBackgroundTask($0) }) {
        beginTask = begin; endTask = end
    }

    func begin(expired: @escaping @MainActor @Sendable () -> Void) {
        guard identifier == .invalid else { return }
        generation += 1
        let version = generation
        identifier = beginTask { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.generation == version, self.identifier != .invalid else { return }
                self.end()
                expired()
            }
        }
    }

    func end() {
        generation += 1
        let previous = identifier
        identifier = .invalid
        if previous != .invalid { endTask(previous) }
    }
}
