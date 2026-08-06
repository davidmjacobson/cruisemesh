import Foundation

let familyRelayRequestIntervalMs: UInt64 = 500
let familyRelayBackoffBaseMs: UInt64 = 1_000
let familyRelayBackoffCapMs: UInt64 = 60_000
let familyRelayJitterWindowMs: UInt64 = 1_000

/// Reserves serial request slots without sleeping, so policy tests use a fake clock.
final class FamilyRelayRequestPacer: @unchecked Sendable {
    private let lock = NSLock()
    private let intervalMs: UInt64
    private var nextRequestAtMs: UInt64 = 0

    init(intervalMs: UInt64 = familyRelayRequestIntervalMs) {
        self.intervalMs = intervalMs
    }

    func reserve(nowMs: UInt64) -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        let requestAtMs = max(nowMs, nextRequestAtMs)
        nextRequestAtMs = requestAtMs + intervalMs
        return requestAtMs - nowMs
    }
}

/// Stable across launches and devices, unlike Swift's process-randomized `hashValue`.
func familyRelayIdentityHash(_ identity: Data) -> UInt64 {
    identity.reduce(UInt64(14_695_981_039_346_656_037)) { hash, byte in
        (hash ^ UInt64(byte)) &* 1_099_511_628_211
    }
}

func familyRelayBackoffDelayMs(
    retryAfterMs: UInt64,
    consecutiveRateLimits: Int,
    identityHash: UInt64
) -> UInt64 {
    let exponent = min(max(consecutiveRateLimits - 1, 0), 6)
    let exponentialMs = min(familyRelayBackoffBaseMs << exponent, familyRelayBackoffCapMs)
    let floorMs = max(retryAfterMs, exponentialMs)
    return floorMs + identityHash % (familyRelayJitterWindowMs + 1)
}

final class FamilyRelayBackoff: @unchecked Sendable {
    private let lock = NSLock()
    private(set) var consecutiveRateLimits = 0

    func onRateLimited(retryAfterMs: UInt64, identityHash: UInt64) -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        consecutiveRateLimits += 1
        return familyRelayBackoffDelayMs(
            retryAfterMs: retryAfterMs,
            consecutiveRateLimits: consecutiveRateLimits,
            identityHash: identityHash
        )
    }

    func onSuccessfulPass() {
        lock.lock()
        consecutiveRateLimits = 0
        lock.unlock()
    }
}

struct FamilyRelayRateLimitAbort: Error {
    let retryDelayMs: UInt64
}
