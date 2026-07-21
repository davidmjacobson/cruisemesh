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
///    has completed and found *zero* peers (`onScanCompleted`). A /24 that
///    finds a peer never arms it -- that peer is proof discovery already
///    works here, so there is no case yet for the wider, costlier sweep.
///  - Once eligible, it fires after a real delay (`emptyLocalSweepFullDelayMs`,
///    default 60s), not immediately, then backs off further
///    (`fullBackoffMs`) each time it runs and still finds nobody.
///  - `onPeerEvidence` resets that backoff, but callers must only invoke it
///    for genuinely NEW peer evidence -- repeated evidence about an
///    already-connected/linked peer (e.g. its Bonjour record refreshing)
///    must not re-trigger sweeps.
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

    private let lock = NSLock()
    private let localIntervalMs: Int64
    private let fullBackoffMs: [Int64]
    private let emptyLocalSweepFullDelayMs: Int64
    private var joined = false
    private var localDueAtMs: Int64 = 0
    /// Armed only once a /24 sweep has completed on this network join and
    /// found nobody. See the class doc.
    private var fullEligible = false
    private var fullDueAtMs: Int64 = 0
    private var fullBackoffIndex = 0

    init(
        localIntervalMs: Int64 = LanScanPlanner.localScanIntervalMs,
        fullBackoffMs: [Int64] = LanScanPlanner.fullScanBackoffMs,
        emptyLocalSweepFullDelayMs: Int64 = LanScanPlanner.emptyLocalSweepFullDelayMs
    ) {
        precondition(!fullBackoffMs.isEmpty)
        self.localIntervalMs = localIntervalMs
        self.fullBackoffMs = fullBackoffMs
        self.emptyLocalSweepFullDelayMs = emptyLocalSweepFullDelayMs
    }

    func onNetworkJoined(nowMs: Int64) {
        lock.lock()
        defer { lock.unlock() }
        joined = true
        localDueAtMs = nowMs
        fullEligible = false
        fullDueAtMs = 0
        fullBackoffIndex = 0
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
    /// reports whether any candidate answered. Only a /24 sweep that found
    /// nobody arms the full-subnet tier for the first time; a /24 sweep
    /// that finds a peer, or one that runs after the tier is already armed,
    /// leaves the existing full-sweep schedule untouched.
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

    /// Evidence a peer is on this network right now. Only meaningful once
    /// the full tier is already eligible (see `onScanCompleted`) -- before
    /// that, evidence doesn't change anything, since the full sweep isn't
    /// on the table yet. Callers are responsible for only calling this for
    /// genuinely NEW evidence.
    func onPeerEvidence(nowMs: Int64) {
        lock.lock()
        defer { lock.unlock() }
        guard joined, fullEligible else { return }
        fullBackoffIndex = 0
        fullDueAtMs = min(fullDueAtMs, nowMs)
    }
}
