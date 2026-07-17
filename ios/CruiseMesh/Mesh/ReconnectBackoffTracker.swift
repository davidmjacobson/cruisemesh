import Foundation

final class ReconnectBackoffTracker {
    static let defaultInitialBackoffMs: Int64 = 5_000
    static let defaultMaxBackoffMs: Int64 = 5 * 60_000
    static let defaultMaxConsecutiveFailures = 6

    private let core: CoreReconnectBackoffTracker

    init(
        initialBackoffMs: Int64 = ReconnectBackoffTracker.defaultInitialBackoffMs,
        maxBackoffMs: Int64 = ReconnectBackoffTracker.defaultMaxBackoffMs,
        maxConsecutiveFailures: Int = ReconnectBackoffTracker.defaultMaxConsecutiveFailures
    ) {
        core = CoreReconnectBackoffTracker(
            initialBackoffMs: initialBackoffMs,
            maxBackoffMs: maxBackoffMs,
            maxFailures: UInt32(maxConsecutiveFailures)
        )
    }

    func canAttempt(address: String, nowMs: Int64) -> Bool { core.canAttempt(address: address, nowMs: nowMs) }
    func isGivenUp(address: String) -> Bool { core.isGivenUp(address: address) }
    func failureCount(address: String) -> Int { Int(core.failureCount(address: address)) }
    func retryDelayMs(address: String, nowMs: Int64) -> Int64? { core.retryDelayMs(address: address, nowMs: nowMs) }
    @discardableResult
    func recordFailure(address: String, nowMs: Int64) -> Int { Int(core.recordFailure(address: address, nowMs: nowMs)) }
    func recordSuccess(address: String) { core.recordSuccess(address: address) }
    func clear() { core.clear() }
}
