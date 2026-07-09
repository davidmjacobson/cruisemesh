import Foundation

final class ReconnectBackoffTracker {
    static let initialBackoffMs: Int64 = 5_000
    static let maxBackoffMs: Int64 = 5 * 60_000
    static let maxConsecutiveFailures = 6

    private struct State {
        var consecutiveFailures: Int
        var nextEligibleAtMs: Int64
    }

    private var state: [String: State] = [:]

    func canAttempt(address: String, nowMs: Int64) -> Bool {
        guard let s = state[address] else { return true }
        if s.consecutiveFailures >= Self.maxConsecutiveFailures { return false }
        return nowMs >= s.nextEligibleAtMs
    }

    func isGivenUp(address: String) -> Bool {
        (state[address]?.consecutiveFailures ?? 0) >= Self.maxConsecutiveFailures
    }

    @discardableResult
    func recordFailure(address: String, nowMs: Int64) -> Int {
        let previous = state[address]?.consecutiveFailures ?? 0
        let failures = previous + 1
        let shift = min(failures - 1, 20)
        let backoff = min(Self.initialBackoffMs << shift, Self.maxBackoffMs)
        state[address] = State(consecutiveFailures: failures, nextEligibleAtMs: nowMs + backoff)
        return failures
    }

    func recordSuccess(address: String) {
        state.removeValue(forKey: address)
    }
}
