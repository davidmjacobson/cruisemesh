import Combine
import Foundation

enum ReachabilityLevel: Int, CaseIterable {
    case nearby
    case onlineRelay
    case recent
    case meshCarry
    case offline
}

enum RelayHealth: Equatable {
    case ok(lastSyncMs: Int64)
    case noInternet
    case noConfig
    case failing(lastAttemptMs: Int64)
    /// The relay answered but rejected our own saved family token (HTTP 401/403).
    case tokenRejected(lastAttemptMs: Int64)
}

enum ContactReachability {
    static let relayPollIntervalMs: Int64 = 60_000
    static let presenceOnlineWindowMs: Int64 = 150_000
    static let recentWindowMs: Int64 = 15 * 60_000

    static func compute(
        directLink: Bool,
        presenceLastSeenMs: Int64?,
        selfRelayHealthy: Bool,
        peerLastSeenMs: Int64?,
        nearbyPeerCount: Int,
        nowMs: Int64,
        meshCarryEnabled: Bool = true
    ) -> ReachabilityLevel {
        if directLink { return .nearby }
        if selfRelayHealthy,
           let seen = presenceLastSeenMs,
           nowMs - seen <= presenceOnlineWindowMs {
            return .onlineRelay
        }
        if let seen = peerLastSeenMs, nowMs - seen <= recentWindowMs { return .recent }
        if meshCarryEnabled && nearbyPeerCount > 0 { return .meshCarry }
        return .offline
    }

    static func selfRelayHealthy(_ health: RelayHealth, nowMs: Int64) -> Bool {
        guard case .ok(let lastSyncMs) = health else { return false }
        return nowMs - lastSyncMs <= 2 * relayPollIntervalMs
    }

    static func chatHeaderCopy(
        _ level: ReachabilityLevel,
        peerLastSeenMs: Int64?,
        nowMs: Int64
    ) -> String {
        switch level {
        case .nearby: return "Nearby via Bluetooth"
        case .onlineRelay: return "Online via relay"
        case .recent:
            let minutes = max(0, (nowMs - (peerLastSeenMs ?? nowMs)) / 60_000)
            return minutes >= 60 ? "Active \(minutes / 60)h ago" : "Active \(minutes)m ago"
        case .meshCarry: return "Nearby phones will carry your message"
        case .offline: return "Offline — will deliver when reachable"
        }
    }

    static func contentDescriptionSuffix(_ level: ReachabilityLevel) -> String? {
        switch level {
        case .nearby: return "Nearby via Bluetooth"
        case .onlineRelay: return "Online via relay"
        case .recent: return "Recently active"
        case .meshCarry: return "Reachable through the mesh"
        case .offline: return nil
        }
    }

    static func contactDetailsCopy(
        _ level: ReachabilityLevel,
        peerLastSeenMs: Int64?,
        presenceLastSeenMs: Int64?,
        nowMs: Int64
    ) -> String {
        let base = chatHeaderCopy(level, peerLastSeenMs: peerLastSeenMs, nowMs: nowMs)
        if let seen = presenceLastSeenMs {
            return "\(base) · Last seen via relay \(ageText(seen, nowMs: nowMs)) ago"
        }
        if let seen = peerLastSeenMs {
            return "\(base) · Last seen \(ageText(seen, nowMs: nowMs)) ago"
        }
        return base
    }

    private static func ageText(_ seenAtMs: Int64, nowMs: Int64) -> String {
        let minutes = max(0, (nowMs - seenAtMs) / 60_000)
        return minutes >= 60 ? "\(minutes / 60)h" : "\(minutes)m"
    }
}

@MainActor
final class MeshConnectivityStatus: ObservableObject {
    static let shared = MeshConnectivityStatus()

    @Published private(set) var nearbyPeerIds: Set<Data> = []
    @Published private(set) var relay: RelayHealth = .noConfig
    @Published private(set) var contactLastSeen: [Data: Int64] = [:]
    @Published private(set) var presenceLastSeen: [Data: Int64] = [:]

    private init() {}

    func refreshNearbyRoutes() {
        nearbyPeerIds = Set(MeshRouter.identifiedRoutes().map(\.userId))
    }

    func setRelayHealth(_ health: RelayHealth) { relay = health }

    func mergeLastSeen(userId: Data, seenAtMs: Int64) {
        if seenAtMs > (contactLastSeen[userId] ?? 0) { contactLastSeen[userId] = seenAtMs }
    }

    func mergePresenceLastSeen(userId: Data, seenAtMs: Int64) {
        if seenAtMs > (presenceLastSeen[userId] ?? 0) {
            presenceLastSeen[userId] = seenAtMs
            mergeLastSeen(userId: userId, seenAtMs: seenAtMs)
        }
    }

    func level(for userId: Data, nowMs: Int64) -> ReachabilityLevel {
        ContactReachability.compute(
            directLink: nearbyPeerIds.contains(userId),
            presenceLastSeenMs: presenceLastSeen[userId],
            selfRelayHealthy: ContactReachability.selfRelayHealthy(relay, nowMs: nowMs),
            peerLastSeenMs: contactLastSeen[userId],
            nearbyPeerCount: nearbyPeerIds.count,
            nowMs: nowMs
        )
    }

    func clear() {
        nearbyPeerIds = []
        relay = .noConfig
        contactLastSeen = [:]
        presenceLastSeen = [:]
    }
}

@MainActor
final class ConnectivityClock: ObservableObject {
    static let shared = ConnectivityClock()
    @Published private(set) var nowMs = Int64(Date().timeIntervalSince1970 * 1_000)
    private var timer: AnyCancellable?

    private init() {
        timer = Timer.publish(every: 30, on: .main, in: .common)
            .autoconnect()
            .sink { [weak self] date in
                self?.nowMs = Int64(date.timeIntervalSince1970 * 1_000)
            }
    }
}
