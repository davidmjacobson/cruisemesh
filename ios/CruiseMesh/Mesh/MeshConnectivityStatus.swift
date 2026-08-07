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
    case checking
    case noInternet
    case noConfig
    case failing(lastAttemptMs: Int64)
    case expired(lastAttemptMs: Int64)
    case suspended(lastAttemptMs: Int64)
    /// The relay answered but rejected our own saved family token (HTTP 401/403).
    case tokenRejected(lastAttemptMs: Int64)
    /// CP2b: the family's hosted storage is full (HTTP 507
    /// `family_quota_exceeded`). Posting fails while fetching keeps working,
    /// so this is reported even when the rest of the sync pass succeeded.
    /// Persistent until the family drains the backlog or it expires.
    case quotaFull(lastAttemptMs: Int64)
    /// CP2b: one queued message exceeds the per-envelope size cap (HTTP 413
    /// `envelope_too_large`) and will never post as-is. Actionable locally;
    /// other messages keep delivering.
    case messageTooLarge(lastAttemptMs: Int64)
    /// CP2b: the service asked us to slow down (HTTP 429 `rate_limited`).
    /// Self-heals within the advertised Retry-After window; never an error
    /// to act on.
    case rateLimited(lastAttemptMs: Int64)

    /// The health one completed sync pass earns. Mirrors Android's
    /// `relayHealthAfterSyncPass` (RelayFaultPolicy.kt): the mailbox-level
    /// faults (quota, oversized, rate-limited) surface even when polling
    /// succeeded, because relayd keeps serving fetches while rejecting
    /// posts. `.passExpired` is in that group too: for relayd's seven-day
    /// `FAMILY_EXPIRY_GRACE_MS` an expired pass still fetches and acks and
    /// only POSTs take the 403, so the success flags read "reachable" for a
    /// week while every new message is rejected. `.passSuspended` and
    /// `.tokenRejected` keep the pre-CP2b precedence because relayd rejects
    /// every op for both, so neither can co-occur with a successful poll.
    /// Classification itself lives in the core (`core/src/relay_status.rs`).
    static func afterSyncPass(
        fault: CoreRelayFault?,
        ownRelaySucceeded: Bool,
        anyRelaySucceeded: Bool,
        nowMs: Int64
    ) -> RelayHealth {
        switch fault {
        case .mailboxFull: return .quotaFull(lastAttemptMs: nowMs)
        case .messageTooLarge: return .messageTooLarge(lastAttemptMs: nowMs)
        case .rateLimited: return .rateLimited(lastAttemptMs: nowMs)
        case .passExpired: return .expired(lastAttemptMs: nowMs)
        default: break
        }
        if ownRelaySucceeded && anyRelaySucceeded { return .ok(lastSyncMs: nowMs) }
        switch fault {
        case .passSuspended: return .suspended(lastAttemptMs: nowMs)
        case .tokenRejected: return .tokenRejected(lastAttemptMs: nowMs)
        default: return .failing(lastAttemptMs: nowMs)
        }
    }

    /// Worst-of fold for the faults one pass observed against our OWN saved
    /// config, using the core's shared ranking so both shells keep the same
    /// answer. `.outage` is deliberately never folded in by the caller -- an
    /// unstructured failure is what the success flags already express.
    static func worseFault(_ current: CoreRelayFault?, _ observed: CoreRelayFault) -> CoreRelayFault {
        guard let current else { return observed }
        return relayFaultRank(fault: observed) > relayFaultRank(fault: current) ? observed : current
    }
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

    /// - Parameter pushHealthy: `RelayPushClient`'s WS push socket is
    ///   currently open (battery, 2026-07-21: `RelayPollPolicy` backs the poll
    ///   off to a 900s safety net while this is true and the app is
    ///   foregrounded, so a stale `RelayHealth.ok`'s `lastSyncMs` no longer
    ///   implies the relay path actually went unhealthy -- it may just mean
    ///   nothing new arrived to poll for). When `true`, freshness is
    ///   considered current regardless of how long ago `lastSyncMs` was -- an
    ///   open push socket is itself live proof the self relay path works.
    ///   Still requires `health` to be `.ok` (the last actual sync attempt
    ///   succeeded); this only overrides the *staleness* check, not a genuine
    ///   last-known failure. Defaults to `false` so every existing call site
    ///   (and the poll-driven fallback while push is down, backgrounded, or
    ///   never connected) keeps today's lastSyncMs-age behavior unchanged.
    ///   Mirrors Android's `ContactReachability.selfRelayHealthy`.
    static func selfRelayHealthy(_ health: RelayHealth, nowMs: Int64, pushHealthy: Bool = false) -> Bool {
        guard case .ok(let lastSyncMs) = health else { return false }
        return pushHealthy || nowMs - lastSyncMs <= 2 * relayPollIntervalMs
    }

    static func chatHeaderCopy(
        _ level: ReachabilityLevel,
        peerLastSeenMs: Int64?,
        nowMs: Int64,
        contactHasInternetDelivery: Bool = true
    ) -> String {
        switch level {
        case .nearby: return "Nearby via Bluetooth"
        case .onlineRelay: return "Online via Shore Pass"
        case .recent:
            let minutes = max(0, (nowMs - (peerLastSeenMs ?? nowMs)) / 60_000)
            return minutes >= 60 ? "Active \(minutes / 60)h ago" : "Active \(minutes)m ago"
        // "will carry" promised an outcome no phone has agreed to yet: the
        // message has only been offered to whoever is nearby.
        case .meshCarry: return "Trying nearby phones"
        // The old copy said "will deliver when reachable" for everyone. For a
        // contact who never shared internet delivery that is a promise the app
        // cannot keep -- a sender posts into the *recipient's* mailbox, and
        // they have none, so no amount of waiting reaches them.
        case .offline:
            return contactHasInternetDelivery
                ? "Waiting to deliver"
                : "Delivers when you're nearby"
        }
    }

    static func contentDescriptionSuffix(_ level: ReachabilityLevel) -> String? {
        switch level {
        case .nearby: return "Nearby via Bluetooth"
        case .onlineRelay: return "Online via Shore Pass"
        case .recent: return "Recently active"
        case .meshCarry: return "Reachable through nearby phones"
        case .offline: return nil
        }
    }

    static func contactDetailsCopy(
        _ level: ReachabilityLevel,
        peerLastSeenMs: Int64?,
        presenceLastSeenMs: Int64?,
        nowMs: Int64,
        contactHasInternetDelivery: Bool = true
    ) -> String {
        let base = chatHeaderCopy(
            level,
            peerLastSeenMs: peerLastSeenMs,
            nowMs: nowMs,
            contactHasInternetDelivery: contactHasInternetDelivery
        )
        var seenText = base
        if let seen = presenceLastSeenMs {
            seenText = "\(base) · Last seen \(ageText(seen, nowMs: nowMs)) ago"
        } else if let seen = peerLastSeenMs {
            seenText = "\(base) · Last seen \(ageText(seen, nowMs: nowMs)) ago"
        }
        // Capability is a durable fact about the friend card, so state it even
        // when the live level already reads well -- it is what a sender needs
        // to know before wondering why nothing ever arrives.
        return contactHasInternetDelivery ? seenText : "\(seenText) · Nearby delivery only"
    }

    private static func ageText(_ seenAtMs: Int64, nowMs: Int64) -> String {
        let minutes = max(0, (nowMs - seenAtMs) / 60_000)
        return minutes >= 60 ? "\(minutes / 60)h" : "\(minutes)m"
    }
}

enum DirectPath: Equatable {
    case bluetooth
    case localWifi
}

@MainActor
final class MeshConnectivityStatus: ObservableObject {
    static let shared = MeshConnectivityStatus()

    @Published private(set) var nearbyPeerIds: Set<Data> = []
    @Published private(set) var directPaths: [Data: DirectPath] = [:]
    @Published private(set) var relay: RelayHealth = .noConfig
    /// Contacts whose friend-card relay endpoint has been written off after
    /// authoritatively rejecting us (core `contact_relay_health`).
    ///
    /// Distinct from `relay`, which is our OWN Shore Pass's health -- both
    /// can be true at once ("my pass is fine, but their card points at a host
    /// that no longer knows them"). Published so the contact sheet can say it
    /// live, instead of a person discovering it from device logs as happened
    /// in the field. Mirrors MeshConnectivityStatus.kt's staleRelayContacts.
    @Published private(set) var staleRelayContacts: Set<Data> = []
    /// `RelayPushClient`'s WS push connection state, mirrored here so
    /// `level(for:)` can feed `ContactReachability.selfRelayHealthy`'s
    /// `pushHealthy` parameter -- battery work backs the relay poll off to a
    /// 900s safety net while push is healthy and foregrounded, so relay-health
    /// freshness can no longer rely on `lastSyncMs` alone (see that function's
    /// doc). `MeshController` is the sole writer, mirroring every other
    /// signal here. Mirrors Android's `MeshConnectivityStatus.pushHealthy`.
    @Published private(set) var pushHealthy: Bool = false
    @Published private(set) var contactLastSeen: [Data: Int64] = [:]
    @Published private(set) var presenceLastSeen: [Data: Int64] = [:]

    private init() {}

    func refreshNearbyRoutes() {
        var paths: [Data: DirectPath] = [:]
        for route in MeshRouter.selectedIdentifiedRoutes() {
            let path: DirectPath = route.transport == .lan ? .localWifi : .bluetooth
            paths[route.userId] = path
        }
        directPaths = paths
        nearbyPeerIds = Set(paths.keys)
    }

    func setRelayHealth(_ health: RelayHealth) { relay = health }

    /// Replaces the whole set each sync pass -- a repaired card must clear as
    /// promptly as a broken one appears.
    func setStaleRelayContacts(_ userIds: Set<Data>) { staleRelayContacts = userIds }
    /// `MeshController` calls this from `RelayPushClient`'s health-change
    /// callback.
    func setPushHealthy(_ healthy: Bool) { pushHealthy = healthy }

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
            selfRelayHealthy: ContactReachability.selfRelayHealthy(relay, nowMs: nowMs, pushHealthy: pushHealthy),
            peerLastSeenMs: contactLastSeen[userId],
            nearbyPeerCount: nearbyPeerIds.count,
            nowMs: nowMs
        )
    }

    func clear() {
        nearbyPeerIds = []
        directPaths = [:]
        relay = .noConfig
        staleRelayContacts = []
        pushHealthy = false
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
