import Foundation

/// Delegating shims over the family relay backpressure policy, which lives in
/// the core (`core/src/session/relay_policy.rs`).
///
/// A CruiseMesh family shares one relay request budget, so how fast a phone may
/// ask and what it does when the relay says "too fast" are protocol decisions,
/// not iOS decisions. This file used to hold the interval, the exponential
/// curve, the cap, the jitter window and the arithmetic joining them, with a
/// second copy of all of it in Kotlin. It now holds none of that: no constant,
/// no formula, no branch. What is left is thread safety at this shell's own
/// call sites and the shape `MeshController` already calls.
///
/// The one behaviour change the hoist carried is the jitter input.
/// `familyRelayIdentityHash` used to live here — a hand-written FNV-1a over the
/// user id, added because Swift's `hashValue` is process-randomized and would
/// not have been stable across launches. It was stable, and it still could not
/// agree with Android, which was hashing the same identity with
/// `ByteArray.contentHashCode()`. The core derives the offset from the public
/// user id under a documented BLAKE2b context instead, so both shells draw from
/// one function — see `RATE-01` in specs/protocol-contract-v1.md.
///
/// Deliberately still here rather than deleted: removing the wrappers and
/// calling core straight from `MeshController` is a separate step, gated on
/// paired-platform canary evidence.

/// Reserves serial request slots without sleeping, so policy tests use a fake
/// clock.
final class FamilyRelayRequestPacer: @unchecked Sendable {
    private let core = CoreFamilyRelayPacer()

    /// - Parameter nowMs: a MONOTONIC reading (`DispatchTime.now()`), not wall
    ///   clock: a pacer that can be rewound by a time correction would hand out
    ///   a wait as long as the correction.
    func reserve(nowMs: Int64) -> Int64 {
        core.reserve(nowMs: nowMs)
    }
}

/// Consecutive-429 counter and the quiet window each refusal earns.
final class FamilyRelayBackoff: @unchecked Sendable {
    private let core = CoreFamilyRelayBackoff()

    var consecutiveRateLimits: Int {
        Int(core.consecutiveRateLimits())
    }

    /// - Parameters:
    ///   - retryAfterMs: the already-clamped advertised window from
    ///     `relayRetryAfterMs`, never a raw header value.
    ///   - identityPublicBytes: this device's public user id, which is what the
    ///     core's stable anti-lockstep offset is derived from. Public on
    ///     purpose: the offset is observable in request timing.
    func onRateLimited(retryAfterMs: UInt64, identityPublicBytes: Data) -> UInt64 {
        core.onRateLimited(retryAfterMs: retryAfterMs, identityPublicBytes: identityPublicBytes)
    }

    func onSuccessfulPass() {
        core.onSuccessfulPass()
    }
}

struct FamilyRelayRateLimitAbort: Error {
    let retryDelayMs: UInt64
}
