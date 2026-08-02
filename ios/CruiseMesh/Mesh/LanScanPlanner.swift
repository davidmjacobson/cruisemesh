import Foundation

enum LanScanBreadth: Equatable {
    case local24
    case fullSubnet
}

/// Pure, thread-safe schedule for foreground automatic LAN fallback sweeps.
/// The transport owns the loneliness and foreground gates; this leaf monitor
/// only decides which breadth is due and advances its cadence.
///
/// The full-subnet tier is expensive (up to a /20, ~4k TCP probes at
/// concurrency 64) and ship/hotel Wi-Fi is exactly where the underlying
/// network tends to be a huge flat subnet, so it is deliberately hard to
/// trigger:
///
///  - It only ever becomes eligible after a /24 sweep on this network join
///    has completed and authenticated *zero* friends (`onScanCompleted`). A
///    /24 that authenticates a friend never arms it -- that friend is proof
///    discovery already works here, so there is no case yet for the wider,
///    costlier sweep. A bare TCP connect deliberately does not count: an
///    unrelated service on the default port must not disarm the tier.
///  - Once eligible, it fires after a real delay (`emptyLocalSweepFullDelayMs`,
///    default 60s), not immediately, then backs off further
///    (`fullBackoffMs`) each time it runs and still finds nobody.
///  - `onPeerEvidence` resets that backoff, but callers must only invoke it
///    for genuinely NEW peer evidence -- repeated evidence about an
///    already-connected/linked peer (e.g. its Bonjour record refreshing)
///    must not re-trigger sweeps. Evidence is also only trusted
///    `maxPeerEvidenceResets` times per network join: the "genuinely new"
///    test is a token another device on the Wi-Fi chooses, so an unbounded
///    reset budget would let anything on a shared network keep every phone in
///    range sweeping back to back. Past the budget the evidence still drives
///    ordinary discovery and connection attempts -- it just stops rewinding
///    the sweep schedule.
final class LanScanPlanner {
    static let localScanIntervalMs: Int64 = 5 * 60_000
    static let fullScanBackoffMs: [Int64] = [
        15 * 60_000,
        60 * 60_000,
        4 * 60 * 60_000,
    ]
    /// Delay before the full sweep first becomes due once an empty /24
    /// sweep arms it. Deliberately not "a couple of seconds": there is no
    /// rush to fire the expensive tier the instant the cheap one comes back
    /// clean.
    static let emptyLocalSweepFullDelayMs: Int64 = 60_000
    /// How many times peer evidence may rewind the full-sweep schedule on one
    /// network join. Matched to the transport's simultaneous-link ceiling
    /// (8): a whole family fleet announcing itself on arrival still gets a
    /// prompt sweep each time, while anything else on the Wi-Fi runs out of
    /// budget long before the expensive tier can be driven back to back.
    static let maxPeerEvidenceResets = 8

    private let lock = NSLock()
    private let localIntervalMs: Int64
    private let fullBackoffMs: [Int64]
    private let emptyLocalSweepFullDelayMs: Int64
    private let maxPeerEvidenceResets: Int
    private var joined = false
    private var localDueAtMs: Int64 = 0
    /// Armed only once a /24 sweep has completed on this network join and
    /// found nobody. See the class doc.
    private var fullEligible = false
    private var fullDueAtMs: Int64 = 0
    private var fullBackoffIndex = 0
    /// How much of this network join's `maxPeerEvidenceResets` budget is spent.
    private var peerEvidenceResets = 0

    init(
        localIntervalMs: Int64 = LanScanPlanner.localScanIntervalMs,
        fullBackoffMs: [Int64] = LanScanPlanner.fullScanBackoffMs,
        emptyLocalSweepFullDelayMs: Int64 = LanScanPlanner.emptyLocalSweepFullDelayMs,
        maxPeerEvidenceResets: Int = LanScanPlanner.maxPeerEvidenceResets
    ) {
        precondition(!fullBackoffMs.isEmpty)
        self.localIntervalMs = localIntervalMs
        self.fullBackoffMs = fullBackoffMs
        self.emptyLocalSweepFullDelayMs = emptyLocalSweepFullDelayMs
        self.maxPeerEvidenceResets = maxPeerEvidenceResets
    }

    func onNetworkJoined(nowMs: Int64) {
        lock.lock()
        defer { lock.unlock() }
        joined = true
        localDueAtMs = nowMs
        fullEligible = false
        fullDueAtMs = 0
        fullBackoffIndex = 0
        peerEvidenceResets = 0
    }

    func onNetworkLost() {
        lock.lock()
        defer { lock.unlock() }
        joined = false
    }

    func takeDueScan(nowMs: Int64) -> LanScanBreadth? {
        lock.lock()
        defer { lock.unlock() }
        guard joined else { return nil }
        if nowMs >= localDueAtMs {
            localDueAtMs = nowMs + localIntervalMs
            return .local24
        }
        if fullEligible, nowMs >= fullDueAtMs {
            fullDueAtMs = nowMs + fullBackoffMs[fullBackoffIndex]
            if fullBackoffIndex < fullBackoffMs.count - 1 {
                fullBackoffIndex += 1
            }
            return .fullSubnet
        }
        return nil
    }

    /// A sweep of `breadth` finished probing every candidate. `foundPeer`
    /// reports whether the sweep authenticated an accepted friend (not
    /// merely whether some TCP service answered). Only a /24 sweep that
    /// authenticated nobody arms the full-subnet tier for the first time;
    /// one that found a friend, or one that runs after the tier is already
    /// armed, leaves the existing full-sweep schedule untouched.
    func onScanCompleted(_ breadth: LanScanBreadth, nowMs: Int64, foundPeer: Bool) {
        lock.lock()
        defer { lock.unlock() }
        guard breadth == .local24 else { return }
        if !fullEligible, !foundPeer {
            fullEligible = true
            fullDueAtMs = nowMs + emptyLocalSweepFullDelayMs
            fullBackoffIndex = 0
        }
    }

    /// Evidence a peer is on this network right now. Callers are responsible
    /// for only calling this for genuinely NEW evidence (see the class doc),
    /// and it is trusted at most `maxPeerEvidenceResets` times per network
    /// join.
    ///
    /// Returns whether this evidence changed the schedule, so the caller
    /// knows whether to bring its own next scan check forward. False once the
    /// budget is spent, and false before the full tier is eligible (see
    /// `onScanCompleted`) -- evidence can't conjure a full sweep out of
    /// nowhere, so there is nothing to hurry towards yet.
    @discardableResult
    func onPeerEvidence(nowMs: Int64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard joined, fullEligible else { return false }
        guard peerEvidenceResets < maxPeerEvidenceResets else { return false }
        peerEvidenceResets += 1
        fullBackoffIndex = 0
        fullDueAtMs = min(fullDueAtMs, nowMs)
        return true
    }

    /// A broad-enough sweep received no TCP response at all, which commonly
    /// means Wi-Fi client isolation. Defer further expensive full sweeps to
    /// the backoff cap until fresh peer evidence or a network join resets the
    /// plan.
    func onIsolationSuspected(nowMs: Int64) {
        lock.lock()
        defer { lock.unlock() }
        guard joined else { return }
        fullBackoffIndex = fullBackoffMs.count - 1
        fullDueAtMs = nowMs + (fullBackoffMs.last ?? 0)
    }
}
