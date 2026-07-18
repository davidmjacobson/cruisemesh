import Foundation

/// Pure exponential-backoff decision logic for `RelayPushClient`'s reconnect
/// loop. Kept dependency-free so it's unit-testable in isolation (see
/// `RelayPushBackoffTests`), mirroring Android's `RelayPushBackoff`
/// (`android/.../relay/RelayPushBackoff.kt`) -- and, like it, deliberately
/// simpler than `ReconnectBackoffTracker`'s BLE reconnect backoff: a relay
/// endpoint doesn't go stale or rotate addresses the way a BLE peer does, so
/// there is no give-up threshold here. Every dropped connection just doubles
/// the wait (capped at `maxBackoffMs`); a successful connect resets it back
/// to `initialBackoffMs`. `MeshController`'s 60s poll (`runRelaySync`, driven
/// by `relayTimer`) is the correctness backstop no matter how long this backs
/// off, so there is nothing worth giving up into.
final class RelayPushBackoff {
    static let defaultInitialBackoffMs: Int64 = 2_000
    static let defaultMaxBackoffMs: Int64 = 60_000

    private let initialBackoffMs: Int64
    private let maxBackoffMs: Int64
    private var consecutiveFailures: Int = 0

    init(
        initialBackoffMs: Int64 = RelayPushBackoff.defaultInitialBackoffMs,
        maxBackoffMs: Int64 = RelayPushBackoff.defaultMaxBackoffMs
    ) {
        self.initialBackoffMs = initialBackoffMs
        self.maxBackoffMs = maxBackoffMs
    }

    /// Milliseconds to wait before the next reconnect attempt, given the current failure streak.
    func nextDelayMs() -> Int64 {
        let shift = min(consecutiveFailures, 20) // avoid overflow on the shift, mirrors the Kotlin port
        return min(initialBackoffMs << shift, maxBackoffMs)
    }

    /// Record a failed/dropped connection, growing the next `nextDelayMs` result.
    func recordFailure() {
        consecutiveFailures += 1
    }

    /// Record a successful connect; resets the backoff back to `initialBackoffMs`.
    func recordSuccess() {
        consecutiveFailures = 0
    }
}
