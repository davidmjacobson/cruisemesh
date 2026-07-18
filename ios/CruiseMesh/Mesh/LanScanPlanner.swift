import Foundation

enum LanScanBreadth: Equatable {
    case local24
    case fullSubnet
}

/// Pure, thread-safe schedule for foreground automatic LAN fallback sweeps.
/// The transport owns the loneliness and foreground gates; this leaf monitor
/// only decides which breadth is due and advances its cadence.
final class LanScanPlanner {
    static let localScanIntervalMs: Int64 = 5 * 60_000
    static let fullScanBackoffMs: [Int64] = [
        15 * 60_000,
        60 * 60_000,
        4 * 60 * 60_000,
    ]

    private let lock = NSLock()
    private let localIntervalMs: Int64
    private let fullBackoffMs: [Int64]
    private var joined = false
    private var localDueAtMs: Int64 = 0
    private var localCompletedSinceJoin = false
    private var fullDueAtMs: Int64 = 0
    private var fullBackoffIndex = 0

    init(
        localIntervalMs: Int64 = LanScanPlanner.localScanIntervalMs,
        fullBackoffMs: [Int64] = LanScanPlanner.fullScanBackoffMs
    ) {
        precondition(!fullBackoffMs.isEmpty)
        self.localIntervalMs = localIntervalMs
        self.fullBackoffMs = fullBackoffMs
    }

    func onNetworkJoined(nowMs: Int64) {
        lock.lock()
        defer { lock.unlock() }
        joined = true
        localDueAtMs = nowMs
        localCompletedSinceJoin = false
        fullDueAtMs = nowMs
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
        if localCompletedSinceJoin, nowMs >= fullDueAtMs {
            fullDueAtMs = nowMs + fullBackoffMs[fullBackoffIndex]
            if fullBackoffIndex < fullBackoffMs.count - 1 {
                fullBackoffIndex += 1
            }
            return .fullSubnet
        }
        return nil
    }

    func onScanCompleted(_ breadth: LanScanBreadth) {
        lock.lock()
        defer { lock.unlock() }
        if breadth == .local24 {
            localCompletedSinceJoin = true
        }
    }

    func onPeerEvidence(nowMs: Int64) {
        lock.lock()
        defer { lock.unlock() }
        fullBackoffIndex = 0
        if joined {
            fullDueAtMs = min(fullDueAtMs, nowMs)
        }
    }
}
