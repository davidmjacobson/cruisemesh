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
    /** Direct BLE link to this contact, HELLO'd, right now. */
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
     *   right now would take the BLE path."
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

    /** §3.3 chat header copy. Probabilistic wording only -- never "delivered instantly". */
    fun chatHeaderCopy(level: ReachabilityLevel, peerLastSeenMs: Long?, nowMs: Long): String = when (level) {
        ReachabilityLevel.NEARBY -> "Nearby via Bluetooth"
        ReachabilityLevel.ONLINE_RELAY -> "Online via relay"
        ReachabilityLevel.RECENT -> {
            val minutes = peerLastSeenMs?.let { ((nowMs - it) / 60_000L).coerceAtLeast(0L) } ?: 0L
            if (minutes >= 60) "Active ${minutes / 60}h ago" else "Active ${minutes}m ago"
        }
        ReachabilityLevel.MESH_CARRY -> "Nearby phones will carry your message"
        ReachabilityLevel.OFFLINE -> "Offline — will deliver when reachable"
    }

    /** §3.1 avatar contentDescription suffix; null means "append nothing" (offline is the silent default). */
    fun contentDescriptionSuffix(level: ReachabilityLevel): String? = when (level) {
        ReachabilityLevel.NEARBY -> "Nearby via Bluetooth"
        ReachabilityLevel.ONLINE_RELAY -> "Online via relay"
        ReachabilityLevel.RECENT -> "Recently active"
        ReachabilityLevel.MESH_CARRY -> "Reachable through the mesh"
        ReachabilityLevel.OFFLINE -> null
    }

    fun selfRelayHealthy(relayHealth: RelayHealth, nowMs: Long): Boolean =
        relayHealth is RelayHealth.Ok && nowMs - relayHealth.lastSyncMs <= 2 * RELAY_POLL_INTERVAL_MS

    fun contactDetailsCopy(
        level: ReachabilityLevel,
        peerLastSeenMs: Long?,
        presenceLastSeenMs: Long?,
        nowMs: Long,
    ): String {
        val base = chatHeaderCopy(level, peerLastSeenMs, nowMs)
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
