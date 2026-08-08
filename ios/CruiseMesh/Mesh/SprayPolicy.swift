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

    /// A plan is built; does it go on the radio?
    ///
    /// When this says no, the caller must not send, must not advance a carried
    /// cursor, and must not record hidden-kind offers — a suppressed offer has
    /// to stay exactly as re-discoverable as it was.
    static func admitPlan(
        peerUserId: Data,
        address: String,
        setDigest: UInt64,
        planBytes: UInt64,
        nowMs: Int64 = SprayPolicy.nowMs
    ) -> CoreSprayAdmission {
        core.admitPlan(
            peerKey: UserIdHex.encode(peerUserId),
            linkKey: address,
            setDigest: setDigest,
            planBytes: planBytes,
            nowMs: nowMs
        )
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

    /// A link went away. Peer cadence deliberately survives it.
    static func forgetLink(address: String) {
        core.forgetLink(linkKey: address)
    }

    /// Mesh stopped; none of this is durable state.
    static func reset() {
        core.clear()
    }
}
