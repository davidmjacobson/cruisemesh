import Foundation

final class LanHealthTracker {
    enum Decision: Equatable {
        case send(UInt64)
        case wait
        case close
    }

    private struct LinkState {
        var pendingNonce: UInt64?
        var sentAtMs: Int64
        var consecutiveTimeouts: Int
    }

    private let timeoutMs: Int64
    private let maxConsecutiveTimeouts: Int
    private var links: [String: LinkState] = [:]

    init(timeoutMs: Int64 = 20_000, maxConsecutiveTimeouts: Int = 3) {
        self.timeoutMs = timeoutMs
        self.maxConsecutiveTimeouts = maxConsecutiveTimeouts
    }

    func next(address: String, nowMs: Int64, nonce: UInt64) -> Decision {
        guard var current = links[address] else {
            links[address] = LinkState(pendingNonce: nonce, sentAtMs: nowMs, consecutiveTimeouts: 0)
            return .send(nonce)
        }
        guard current.pendingNonce != nil else {
            current.pendingNonce = nonce
            current.sentAtMs = nowMs
            links[address] = current
            return .send(nonce)
        }
        guard nowMs - current.sentAtMs >= timeoutMs else { return .wait }
        current.consecutiveTimeouts += 1
        if current.consecutiveTimeouts >= maxConsecutiveTimeouts {
            links.removeValue(forKey: address)
            return .close
        }
        current.pendingNonce = nonce
        current.sentAtMs = nowMs
        links[address] = current
        return .send(nonce)
    }

    func response(address: String, nonce: UInt64, nowMs: Int64) -> Int64? {
        guard var current = links[address], current.pendingNonce == nonce else { return nil }
        let latency = max(0, nowMs - current.sentAtMs)
        current.pendingNonce = nil
        current.sentAtMs = 0
        current.consecutiveTimeouts = 0
        links[address] = current
        return latency
    }

    func remove(address: String) {
        links.removeValue(forKey: address)
    }

    func clear() {
        links.removeAll()
    }
}
