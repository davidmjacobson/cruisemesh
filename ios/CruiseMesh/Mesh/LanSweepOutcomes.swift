import Foundation
import Network

/// How one subnet-sweep probe finished. Mirrors the Android classification so
/// both shells reach the same verdict on the same network.
enum LanSweepProbeOutcome: Equatable {
    case connected
    case refused
    case timedOut
    case denied
    case other
}

/// Tally of every probe outcome in one sweep.
struct LanSweepOutcomeSummary: Equatable {
    var connected = 0
    var refused = 0
    var timedOut = 0
    var denied = 0
    var other = 0

    var probed: Int { connected + refused + timedOut + denied + other }

    mutating func record(_ outcome: LanSweepProbeOutcome) {
        switch outcome {
        case .connected: connected += 1
        case .refused: refused += 1
        case .timedOut: timedOut += 1
        case .denied: denied += 1
        case .other: other += 1
        }
    }

    func logLine(prefixLength: Int) -> String {
        "Sweep complete (/\(prefixLength)): \(probed) probed, \(connected) connected, "
            + "\(refused) refused, \(timedOut) timed out, \(denied) denied, \(other) other."
    }
}

enum LanSweepVerdict: Equatable {
    case isolationSuspected
    case blockedByPolicy
    case healthyButEmpty
    case foundPeer
    case inconclusive
}

/// What the diagnostics screen says about the last sweep.
enum LanSweepDisplayState: Equatable {
    case none
    case checking
    case isolationSuspected
    case blockedByPolicy
}

/// A sweep narrower than this cannot distinguish "client isolation" from
/// "only a handful of addresses were even tried".
private let minIsolationSweepCandidates = 253

/// Pure policy decision for the user-facing result and the planner reaction.
func lanSweepVerdict(_ summary: LanSweepOutcomeSummary) -> LanSweepVerdict {
    if summary.connected > 0 { return .foundPeer }
    // Policy denial is more specific than all-silent isolation and must not
    // change scheduling: a VPN can deny every socket before it reaches Wi-Fi.
    if summary.denied > 0 { return .blockedByPolicy }
    if summary.probed >= minIsolationSweepCandidates, summary.refused == 0 {
        return .isolationSuspected
    }
    if summary.refused > 0 { return .healthyButEmpty }
    return .inconclusive
}

/// Classifies the terminal error of one scan probe. The scan's own timeout
/// path reports `.timedOut` directly and never reaches here.
func classifyLanSweepProbeFailure(_ error: NWError?) -> LanSweepProbeOutcome {
    guard let error else { return .other }
    // EPERM and the DNS-SD policy-denied code are exactly the "the OS or a
    // VPN refused to let this socket out" signals; reuse the shared matcher
    // so the two stay in step.
    if isKnownLocalNetworkPermissionError(error) { return .denied }
    guard case let .posix(code) = error else { return .other }
    switch code {
    case .ECONNREFUSED:
        return .refused
    case .ETIMEDOUT:
        return .timedOut
    case .EACCES:
        return .denied
    default:
        return .other
    }
}

/// Thread-safe state holder for the LAN sweep result shown in diagnostics.
///
/// A verdict can only enter the display state through `onSweepCompleted`,
/// which the transport calls only after every candidate has retired. Network
/// changes and peer evidence synchronously replace any previous verdict so
/// diagnostics never describe a stale network.
final class LanSweepDisplayTracker {
    private let lock = NSLock()
    private var state = LanSweepDisplayState.none
    private var peerSeenOnNetwork = false

    @discardableResult
    func onNetworkJoined() -> LanSweepDisplayState {
        lock.lock()
        defer { lock.unlock() }
        peerSeenOnNetwork = false
        return set(.checking)
    }

    @discardableResult
    func onNetworkLost() -> LanSweepDisplayState {
        lock.lock()
        defer { lock.unlock() }
        peerSeenOnNetwork = false
        return set(.none)
    }

    @discardableResult
    func onSweepStarted() -> LanSweepDisplayState {
        lock.lock()
        defer { lock.unlock() }
        return set(.checking)
    }

    @discardableResult
    func onSweepCompleted(_ summary: LanSweepOutcomeSummary) -> LanSweepDisplayState {
        lock.lock()
        defer { lock.unlock() }
        guard !peerSeenOnNetwork else { return set(.none) }
        switch lanSweepVerdict(summary) {
        case .isolationSuspected:
            return set(.isolationSuspected)
        case .blockedByPolicy:
            return set(.blockedByPolicy)
        default:
            return set(.none)
        }
    }

    @discardableResult
    func onPeerEvidence() -> LanSweepDisplayState {
        lock.lock()
        defer { lock.unlock() }
        peerSeenOnNetwork = true
        return set(.none)
    }

    func current() -> LanSweepDisplayState {
        lock.lock()
        defer { lock.unlock() }
        return state
    }

    private func set(_ next: LanSweepDisplayState) -> LanSweepDisplayState {
        state = next
        return next
    }
}
