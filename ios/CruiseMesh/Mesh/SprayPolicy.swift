import Foundation

/// Process-wide delegate to the core digest-spray policy (Android
/// `SprayPolicy.kt` twin).
///
/// There is deliberately no decision in this file. Every question this shell
/// used to answer with a local timestamp — may this peer be sprayed, how much
/// may go out, is this the same offer we just made, how long should a quiet
/// link wait — is answered by `core/src/spray_policy.rs`, so iOS and Android
/// cannot drift. What lives here is the address-to-key mapping, the clock
/// choice, and nothing else.
///
/// ## Keys
///
/// - `peerUserId` becomes the *logical* peer key. Cadence is per peer because
///   reconnect churn moves between addresses; keying it by address would let a
///   phone reconnecting on a new address walk straight past the gate.
/// - `address` is the link key. The byte budget is per link because the FIFO
///   being filled belongs to a link, not to a peer.
///
/// ## Clock
///
/// `FailoverResumeDebounce.monotonicNowMs`, never wall-clock time. The map
/// this replaces used `Date().timeIntervalSince1970`, so a time correction
/// landing mid-session could expire a spray window early — producing exactly
/// the burst the window exists to prevent — or hold it open indefinitely. It
/// is also the clock the failover debounce and every `asyncAfter` count on, so
/// all three brakes now measure time the same way.
enum SprayPolicy {
    private static let core = CoreSprayPolicy()

    /// The shared Rust policy itself, for `MessageStore.corePlanMeshMeet`,
    /// which takes the cadence verdict and charges the burst allowance inside
    /// one planning call. It has to be this object: a planner given a fresh
    /// policy would forget every window the moment it returned.
    static var coreState: CoreSprayPolicy { core }

    /// Monotonic milliseconds; see the type doc.
    static var nowMs: Int64 { FailoverResumeDebounce.monotonicNowMs }

    /// May this peer be sprayed now, and with what per-lane byte budgets?
    ///
    /// Consulted before any store work, so a reconnect storm costs a map
    /// lookup rather than a full plan build. Records nothing: a burst that is
    /// then deferred must not arm a cadence it never spent.
    static func maySpray(
        peerUserId: Data,
        address: String,
        trigger: CoreSprayTrigger,
        nowMs: Int64 = SprayPolicy.nowMs
    ) -> CoreSprayGate {
        core.maySpray(
            peerKey: UserIdHex.encode(peerUserId),
            linkKey: address,
            trigger: trigger,
            nowMs: nowMs
        )
    }

    /// A DIGEST frame actually went out to this peer on this link.
    static func noteDigestSent(
        peerUserId: Data,
        address: String,
        nowMs: Int64 = SprayPolicy.nowMs
    ) {
        core.noteDigestSent(
            peerKey: UserIdHex.encode(peerUserId),
            linkKey: address,
            nowMs: nowMs
        )
    }

    /// A plan is built; which of its lanes go on the radio?
    ///
    /// Per lane, not per plan: the recorded shape was an invariant authored set
    /// beside a carried set walking a cursor, and one digest over all three
    /// would change on every page turn and so suppress nothing.
    ///
    /// When a lane is refused the caller must not send it, must not advance a
    /// carried cursor, and must not record hidden-kind offers — a suppressed
    /// offer has to stay exactly as re-discoverable as it was.
    static func admitPlan(
        peerUserId: Data,
        address: String,
        lanes: CoreSprayPlanShape,
        nowMs: Int64 = SprayPolicy.nowMs
    ) -> CoreSprayAdmission {
        core.admitPlan(
            peerKey: UserIdHex.encode(peerUserId),
            linkKey: address,
            lanes: lanes,
            nowMs: nowMs
        )
    }

    /// Bytes this encounter queued at `address` outside a spray plan: the
    /// receipt repair pass, the per-missing-message re-send loop, the group
    /// catch-up and the carry drain. Pure accounting — it refuses nothing, it
    /// changes what the next `maySpray` sees.
    static func noteBytesQueued(
        address: String,
        bytes: Int,
        nowMs: Int64 = SprayPolicy.nowMs
    ) {
        guard bytes > 0 else { return }
        core.noteBytesQueued(linkKey: address, bytes: UInt64(bytes), nowMs: nowMs)
    }

    /// Evidence that sprays toward this peer are achieving something: carried
    /// copies it confirmed holding, or a receipt consumed from it. Resets the
    /// receipt-quiet backoff.
    static func noteReceiptProgress(
        peerUserId: Data,
        nowMs: Int64 = SprayPolicy.nowMs
    ) {
        core.noteReceiptProgress(peerKey: UserIdHex.encode(peerUserId), nowMs: nowMs)
    }

    /// Longest deferral worth arming a timer for, from core.
    static var retryArmMaxMs: Int64 { coreSprayRetryArmMaxMs() }

    /// A link went away. Nothing is reset: neither the peer's cadence nor this
    /// link's burst allowance. A disconnect is what reconnect churn produces —
    /// hundreds per hour in the field — so clearing either on one would hand
    /// the churn back the bound it defeats.
    static func noteLinkClosed(address: String, nowMs: Int64 = SprayPolicy.nowMs) {
        core.noteLinkClosed(linkKey: address, nowMs: nowMs)
    }

    /// Route this policy's decisions into the store's protocol-event ring, so
    /// a shared diagnostics archive can show why a peer was sprayed,
    /// suppressed or backed off.
    ///
    /// Idempotent, and safe to call late: an unattached policy behaves exactly
    /// as it did before the ring existed. Core builds the records and redacts
    /// them; nothing here composes an event.
    static func attachEventJournal(store: MessageStore) {
        core.attachEventJournal(store: store)
    }

    /// Mesh stopped; none of this is durable state.
    static func reset() {
        core.clear()
    }
}
