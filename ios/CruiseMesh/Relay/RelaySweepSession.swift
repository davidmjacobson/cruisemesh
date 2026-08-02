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

/// Contacts whose friend-card relay endpoint has stopped answering this
/// process, and for how many consecutive sync passes.
///
/// The counterpart of the persisted rejection streaks in core
/// `contact_relay_health`, for the half of the failure that produces no HTTP
/// answer to classify. A revoked token replies 401; a *retired host* replies
/// nothing at all, and that transport failure never reached the rejection
/// streak, so the address was re-dialled on every pass indefinitely.
///
/// In memory rather than in the store, on both shells, for two reasons. A host
/// that is down is usually down for minutes, so re-learning it after a restart
/// costs two passes and is cheaper than carrying a stale verdict across days.
/// And it keeps "not answering right now" out of the persisted stale-card set
/// the contact sheet reads, where the prompt is "ask them to share their card
/// again" -- correct for a revoked token, wrong for a relay that is rebooting.
///
/// Mirrors `RelaySyncEngine.contactRelayUnreachable` on Android.
final class ContactRelaySilence: @unchecked Sendable {
    static let shared = ContactRelaySilence()

    private struct State {
        var streak: Int64
        var restedAtMs: Int64
    }

    private let lock = NSLock()
    private var silent: [Data: State] = [:]

    /// Whether this contact's endpoint has answered recently enough to be
    /// worth spending a request on. True below the core's streak, and true
    /// again once the rest window is up so a recovered host is picked back up
    /// with nobody touching the phone.
    func endpointAnswering(userId: Data, nowMs: Int64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let state = silent[userId] else { return true }
        return coreContactRelayUnreachableEndpointUsable(
            unreachableStreak: state.streak,
            restedAtMs: state.restedAtMs,
            nowMs: nowMs
        )
    }

    /// Records one whole pass in which this endpoint said nothing while
    /// another relay answered. Returns the new streak.
    @discardableResult
    func noteSilentPass(userId: Data, nowMs: Int64) -> Int64 {
        lock.lock()
        defer { lock.unlock() }
        let streak = (silent[userId]?.streak ?? 0)
            + coreContactRelayUnreachableDelta(otherRelayAnswered: true)
        silent[userId] = State(streak: streak, restedAtMs: nowMs)
        return streak
    }

    /// The endpoint answered: whatever we thought about its silence is settled.
    func noteAnswered(userId: Data) {
        lock.lock()
        defer { lock.unlock() }
        silent[userId] = nil
    }

    /// Test seam: forget everything, as though the process had just started.
    func reset() {
        lock.lock()
        defer { lock.unlock() }
        silent.removeAll()
    }
}
