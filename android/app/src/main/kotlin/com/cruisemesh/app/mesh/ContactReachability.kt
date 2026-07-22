package com.cruisemesh.app.mesh

/**
 * How likely a message to this contact is to
 * be delivered promptly, at a glance. Precedence is declaration order --
 * [ReachabilityLevel.NEARBY] beats [ReachabilityLevel.ONLINE_RELAY] beats
 * [ReachabilityLevel.RECENT] beats [ReachabilityLevel.MESH_CARRY] beats
 * [ReachabilityLevel.OFFLINE] -- so callers picking the "best" of several
 * levels (e.g. a group's member levels) can just compare `ordinal`.
 */
enum class ReachabilityLevel {
    /** Direct link (BLE or LAN) to this contact, HELLO'd, right now. */
    NEARBY,

    /** Their device synced with the relay very recently AND our relay path works. */
    ONLINE_RELAY,

    /** Heard from them (BLE or relay presence) within [ContactReachability.RECENT_WINDOW_MS]. */
    RECENT,

    /** Not reachable directly, but >=1 HELLO'd BLE peer will mule the message. */
    MESH_CARRY,

    /** None of the above -- message will queue for later delivery. */
    OFFLINE,
}

/**
 * Pure, unit-testable reachability computation, same no-Android-deps
 * pattern as [ReconnectBackoffTracker] /
 * [MeshRouterState]. Callers (Compose layer) supply a snapshot of
 * [MeshConnectivityStatus] plus an injected clock; this class holds no state
 * of its own.
 */
object ContactReachability {
    /** Existing relay poll cadence; duplicated here so UI reachability logic stays pure/testable. */
    const val RELAY_POLL_INTERVAL_MS = 60_000L

    /** 2.5x the 60 s relay poll: "their phone is actively syncing right now." */
    const val PRESENCE_ONLINE_WINDOW_MS = 150_000L

    /** "Was around a moment ago; delivery likely soon, not instant." */
    const val RECENT_WINDOW_MS = 15 * 60_000L

    /**
     * Gates [ReachabilityLevel.MESH_CARRY]. Off until 1:1 BLE muling
     * actually ships -- otherwise "some peer nearby"
     * doesn't move a 1:1 message and the tier would be a lie. That fix
     * shipped 2026-07-11 (agent/ble-1to1-muling), so this is on.
     */
    const val MESH_CARRY_ENABLED = true

    /**
     * @param directLink `MeshRouter.routeFor(userId) != null` -- must come
     *   from the same lookup the send path uses, so NEARBY means "a send
     *   right now would take a direct BLE-or-LAN link" (see
     *   [chatHeaderCopy]'s `transport` param for which one).
     * @param presenceLastSeenMs relay-presence last-seen for this contact.
     *   Relay presence is kept separate from
     *   general last-seen evidence so only actual relay presence can light up
     *   [ReachabilityLevel.ONLINE_RELAY].
     * @param selfRelayHealthy our own last relay sync pass succeeded
     *   recently and we have validated internet.
     * @param peerLastSeenMs max of: relay presence, last HELLO on any link,
     *   last message/receipt received from this contact.
     * @param nearbyPeerCount count of distinct HELLO'd peers (any contact or
     *   stranger) -- MESH_CARRY doesn't require the peer itself to be a
     *   contact, since post-muling a stranger phone is a valid carrier too.
     */
    fun compute(
        directLink: Boolean,
        presenceLastSeenMs: Long?,
        selfRelayHealthy: Boolean,
        peerLastSeenMs: Long?,
        nearbyPeerCount: Int,
        nowMs: Long,
        meshCarryEnabled: Boolean = MESH_CARRY_ENABLED,
    ): ReachabilityLevel = when {
        directLink -> ReachabilityLevel.NEARBY
        selfRelayHealthy && presenceLastSeenMs != null && nowMs - presenceLastSeenMs <= PRESENCE_ONLINE_WINDOW_MS ->
            ReachabilityLevel.ONLINE_RELAY
        peerLastSeenMs != null && nowMs - peerLastSeenMs <= RECENT_WINDOW_MS -> ReachabilityLevel.RECENT
        meshCarryEnabled && nearbyPeerCount > 0 -> ReachabilityLevel.MESH_CARRY
        else -> ReachabilityLevel.OFFLINE
    }

    /** NEARBY copy for the live transport a send to this contact would actually take; null when unknown. */
    private fun nearbyViaCopy(transport: MeshRouterState.Transport?): String = when (transport) {
        MeshRouterState.Transport.LAN -> "Nearby via Wi-Fi"
        MeshRouterState.Transport.CENTRAL, MeshRouterState.Transport.PERIPHERAL -> "Nearby via Bluetooth"
        null -> "Nearby"
    }

    /**
     * §3.3 chat header copy. Probabilistic wording only -- never "delivered
     * instantly".
     *
     * @param transport the live route's transport for [ReachabilityLevel.NEARBY]
     *   (e.g. `MeshRouter.routeFor(userId)?.first`) -- the direct link may be
     *   BLE or LAN, so this must not be assumed. Null falls back to a
     *   transport-neutral "Nearby" (unknown transport, or non-NEARBY levels
     *   where it's unused).
     */
    fun chatHeaderCopy(
        level: ReachabilityLevel,
        peerLastSeenMs: Long?,
        nowMs: Long,
        transport: MeshRouterState.Transport? = null,
    ): String = when (level) {
        ReachabilityLevel.NEARBY -> nearbyViaCopy(transport)
        ReachabilityLevel.ONLINE_RELAY -> "Online via relay"
        ReachabilityLevel.RECENT -> {
            val minutes = peerLastSeenMs?.let { ((nowMs - it) / 60_000L).coerceAtLeast(0L) } ?: 0L
            if (minutes >= 60) "Active ${minutes / 60}h ago" else "Active ${minutes}m ago"
        }
        ReachabilityLevel.MESH_CARRY -> "Nearby phones will carry your message"
        ReachabilityLevel.OFFLINE -> "Offline — will deliver when reachable"
    }

    /**
     * §3.1 avatar contentDescription suffix; null means "append nothing"
     * (offline is the silent default). See [chatHeaderCopy]'s [transport] doc.
     */
    fun contentDescriptionSuffix(level: ReachabilityLevel, transport: MeshRouterState.Transport? = null): String? = when (level) {
        ReachabilityLevel.NEARBY -> nearbyViaCopy(transport)
        ReachabilityLevel.ONLINE_RELAY -> "Online via relay"
        ReachabilityLevel.RECENT -> "Recently active"
        ReachabilityLevel.MESH_CARRY -> "Reachable through the mesh"
        ReachabilityLevel.OFFLINE -> null
    }

    /**
     * @param pushHealthy `RelayPushClient`'s WS push socket is currently open
     *   ([RadioPowerPolicy]'s battery work backs the poll off to a 900s
     *   safety net while this is true, so a stale [RelayHealth.Ok.lastSyncMs]
     *   no longer implies the relay path actually went unhealthy -- it may
     *   just mean nothing new arrived to poll for). When true, freshness is
     *   considered current regardless of how long ago [relayHealth]'s
     *   `lastSyncMs` was -- an open push socket is itself live proof the self
     *   relay path works. Still requires [relayHealth] to be
     *   [RelayHealth.Ok] (the last actual sync attempt succeeded); this only
     *   overrides the *staleness* check, not a genuine last-known failure.
     *   Defaults to `false` so every existing call site (and the poll-driven
     *   fallback while push is down or hasn't connected) keeps today's
     *   lastSyncMs-age behavior unchanged.
     */
    fun selfRelayHealthy(relayHealth: RelayHealth, nowMs: Long, pushHealthy: Boolean = false): Boolean =
        relayHealth is RelayHealth.Ok &&
            (pushHealthy || nowMs - relayHealth.lastSyncMs <= 2 * RELAY_POLL_INTERVAL_MS)

    fun contactDetailsCopy(
        level: ReachabilityLevel,
        peerLastSeenMs: Long?,
        presenceLastSeenMs: Long?,
        nowMs: Long,
        transport: MeshRouterState.Transport? = null,
    ): String {
        val base = chatHeaderCopy(level, peerLastSeenMs, nowMs, transport)
        return when {
            presenceLastSeenMs != null -> "$base · Last seen via relay ${ageText(presenceLastSeenMs, nowMs)} ago"
            peerLastSeenMs != null -> "$base · Last seen ${ageText(peerLastSeenMs, nowMs)} ago"
            else -> base
        }
    }

    private fun ageText(seenAtMs: Long, nowMs: Long): String {
        val minutes = ((nowMs - seenAtMs) / 60_000L).coerceAtLeast(0L)
        return if (minutes >= 60) "${minutes / 60}h" else "${minutes}m"
    }
}
