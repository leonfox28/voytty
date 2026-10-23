import SwiftUI
import UIKit

/// Keep an existing interactive connection running during a short app switch.
/// One request per background visit; expiration never starts another assertion
/// or detaches the terminal. iOS owns the available time and may deny the request.
@MainActor
final class BackgroundConnection {
    private let work: BackgroundWork
    private var requested = false
    var isActive: Bool { work.isActive }

    init(work: BackgroundWork? = nil) {
        self.work = work ?? BackgroundWork(begin: {
            UIApplication.shared.beginBackgroundTask(withName: "Terminal connection", expirationHandler: $0)
        })
    }

    func update(phase: ScenePhase, ownsConnection: Bool) {
        if phase == .active {
            requested = false
            work.end()
            return
        }
        guard ownsConnection else {
            work.end()
            return
        }
        // Inactive also covers system prompts and the transition back to the
        // foreground. Do not start a task for prompts or release an existing
        // background assertion until the app is actually active again.
        guard phase == .background, !requested else { return }
        requested = true
        work.begin(expired: {})
    }
}
