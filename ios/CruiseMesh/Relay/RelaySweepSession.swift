import Foundation
import os.log

/// Logger for the relay sync pass's off-main-actor work. `MeshController.log`
/// is an instance property of a `@MainActor` type; the fetch walk runs
/// `nonisolated`, so it needs a logger it can reach without hopping to the
/// main actor just to say something went wrong.
let relaySyncLog = Logger(subsystem: "com.cruisemesh", category: "MeshController")

/// What a `/ws` subscribe needs: which recipient hints to watch, and where in
/// the mailbox to start from. Mirrors Android's `RelayPushSubscription`.
struct RelayPushSubscription {
    let hints: [Data]
    let afterId: Int64
}

/// Which relay mailboxes this *process* has already walked in full.
///
/// The counterpart of `RelaySyncEngine.sweptThisSession` on Android, and
/// deliberately in-memory on both: `relaySweepDue` treats the first pass after
/// a cold start as a sweep, which is the self-healing answer to a persisted
/// fetch frontier that has gone stale in a way no relay response can reveal --
/// most importantly a relay rebuilt from scratch, whose row ids restart at 1
/// and would otherwise sit forever below a frontier we still remember.
/// Restarting the app repairs it immediately; the six-hourly sweep repairs it
/// unattended.
///
/// Keys are `relayCursorKey(relayUrl:relayToken:)` values -- hashes, never
/// credentials.
final class RelaySweepSession: @unchecked Sendable {
    static let shared = RelaySweepSession()

    private let lock = NSLock()
    private var swept: Set<String> = []

    /// Whether a full walk of this mailbox has completed since process start.
    func hasSwept(_ configKey: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return swept.contains(configKey)
    }

    /// Records that a walk from 0 reached the end of this mailbox. Called only
    /// on natural termination -- a sweep cut short by a relay error or a lost
    /// network leaves this untouched, so the next pass tries again rather than
    /// believing a partial re-walk was a full one.
    func noteSwept(_ configKey: String) {
        lock.lock()
        defer { lock.unlock() }
        swept.insert(configKey)
    }

    /// Test seam: forget everything, as though the process had just started.
    func reset() {
        lock.lock()
        defer { lock.unlock() }
        swept.removeAll()
    }
}
