import Foundation

/**
 Per-address exponential backoff for BLE central reconnect attempts.
 Framework-free so it is unit-testable (Android `ReconnectBackoffTracker` parity).
 */
final class ReconnectBackoffTracker {
    static let defaultInitialBackoffMs: Int64 = 5_000
    static let defaultMaxBackoffMs: Int64 = 5 * 60_000
    static let defaultMaxConsecutiveFailures = 6

    private let initialBackoffMs: Int64
    private let maxBackoffMs: Int64
    private let maxConsecutiveFailures: Int

    private struct State {
        var consecutiveFailures: Int
        var nextEligibleAtMs: Int64
    }

    private var state: [String: State] = [:]

    init(
        initialBackoffMs: Int64 = ReconnectBackoffTracker.defaultInitialBackoffMs,
        maxBackoffMs: Int64 = ReconnectBackoffTracker.defaultMaxBackoffMs,
        maxConsecutiveFailures: Int = ReconnectBackoffTracker.defaultMaxConsecutiveFailures
    ) {
        self.initialBackoffMs = initialBackoffMs
        self.maxBackoffMs = maxBackoffMs
        self.maxConsecutiveFailures = maxConsecutiveFailures
    }

    func canAttempt(address: String, nowMs: Int64) -> Bool {
        guard let s = state[address] else { return true }
        if s.consecutiveFailures >= maxConsecutiveFailures { return false }
        return nowMs >= s.nextEligibleAtMs
    }

    func isGivenUp(address: String) -> Bool {
        (state[address]?.consecutiveFailures ?? 0) >= maxConsecutiveFailures
    }

    func failureCount(address: String) -> Int {
        state[address]?.consecutiveFailures ?? 0
    }

    func retryDelayMs(address: String, nowMs: Int64) -> Int64? {
        guard let state = state[address],
              state.consecutiveFailures < maxConsecutiveFailures else { return nil }
        return max(0, state.nextEligibleAtMs - nowMs)
    }

    @discardableResult
    func recordFailure(address: String, nowMs: Int64) -> Int {
        let previous = state[address]?.consecutiveFailures ?? 0
        let failures = previous + 1
        let shift = min(failures - 1, 20)
        let backoff = min(initialBackoffMs << shift, maxBackoffMs)
        state[address] = State(consecutiveFailures: failures, nextEligibleAtMs: nowMs + backoff)
        return failures
    }

    func recordSuccess(address: String) {
        state.removeValue(forKey: address)
    }

    func clear() {
        state.removeAll()
    }
}
