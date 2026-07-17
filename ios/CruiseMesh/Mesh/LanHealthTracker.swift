import Foundation

final class LanHealthTracker {
    enum Decision: Equatable { case send(UInt64), wait, close }
    private let core: CoreLanHealthTracker

    init(timeoutMs: Int64 = 20_000, maxConsecutiveTimeouts: Int = 3) {
        core = CoreLanHealthTracker(timeoutMs: timeoutMs, maxTimeouts: UInt32(maxConsecutiveTimeouts))
    }

    func next(address: String, nowMs: Int64, nonce: UInt64) -> Decision {
        let decision = core.next(address: address, nowMs: nowMs, nonce: nonce)
        switch decision.action {
        case .send: return .send(decision.nonce!)
        case .wait: return .wait
        case .close: return .close
        }
    }

    func response(address: String, nonce: UInt64, nowMs: Int64) -> Int64? {
        core.response(address: address, nonce: nonce, nowMs: nowMs)
    }
    func remove(address: String) { core.remove(address: address) }
    func clear() { core.clear() }
}
