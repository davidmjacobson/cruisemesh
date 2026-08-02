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
/// deliberately in-memory on both -- but deliberately *narrow* on both too.
/// `relaySweepDue` schedules from the persisted sweep timestamp and consults
/// this only for a mailbox that has never recorded a completed sweep at all,
/// where it stops a store write that keeps failing from turning every pass
/// into a full walk. A cold start on a mailbox with a recent sweep no longer
/// re-walks anything: a sweep re-downloads the sealed body of every row still
/// in the mailbox, and tying that to the process lifetime made the six-hourly
/// interval meaningless on a phone that restarts its mesh service all day.
/// The cost is that a relay rebuilt from scratch -- row ids restarting at 1
/// under a frontier we still remember -- heals on the interval rather than at
/// the next app restart.
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

    /// `endpointKey` is `relayCursorKey(relayUrl:relayToken:)` -- a hash,
    /// never the credential -- for the endpoint that was silent, so a rest is
    /// tied to the address that earned it rather than to the person.
    private struct State {
        var endpointKey: String
        var streak: Int64
        var restedAtMs: Int64
    }

    private let lock = NSLock()
    private var silent: [Data: State] = [:]

    /// Whether this contact's endpoint has answered recently enough to be
    /// worth spending a request on. True below the core's streak, and true
    /// again once the rest window is up so a recovered host is picked back up
    /// with nobody touching the phone.
    ///
    /// Also true the moment the contact's endpoint *moves*, which is the same
    /// rule core applies to the persisted rejection streak: a new friend card
    /// or a T23 relay-update notice that changes the address gives it a clean
    /// slate, because a host that has never been tried cannot have been
    /// silent. Without this a contact who migrated to a working relay would
    /// keep being skipped for the rest of the half-hour window. Re-importing a
    /// card that re-states the *same* endpoint changes nothing, exactly as it
    /// does not launder a rejection streak.
    func endpointAnswering(userId: Data, endpointKey: String, nowMs: Int64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let state = silent[userId] else { return true }
        guard state.endpointKey == endpointKey else {
            silent[userId] = nil
            return true
        }
        return coreContactRelayUnreachableEndpointUsable(
            unreachableStreak: state.streak,
            restedAtMs: state.restedAtMs,
            nowMs: nowMs
        )
    }

    /// Records one whole pass in which this endpoint said nothing.
    ///
    /// `otherRelayAnswered` is passed through to the core rather than tested
    /// here: without same-pass proof that a different relay answered this
    /// device, the core's delta is 0 and nothing is recorded, because the
    /// failure is then most likely our own connectivity -- a phone in a tunnel
    /// fails every endpoint at once. Returns the new streak, or nil when the
    /// observation was not counted.
    @discardableResult
    func noteSilentPass(
        userId: Data,
        endpointKey: String,
        otherRelayAnswered: Bool,
        nowMs: Int64
    ) -> Int64? {
        lock.lock()
        defer { lock.unlock() }
        let delta = coreContactRelayUnreachableDelta(otherRelayAnswered: otherRelayAnswered)
        guard delta != 0 else { return nil }
        // A rest recorded against a different address says nothing about this
        // one, so the streak restarts rather than resuming.
        var prior: Int64 = 0
        if let state = silent[userId], state.endpointKey == endpointKey {
            prior = state.streak
        }
        let streak = prior + delta
        silent[userId] = State(endpointKey: endpointKey, streak: streak, restedAtMs: nowMs)
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
