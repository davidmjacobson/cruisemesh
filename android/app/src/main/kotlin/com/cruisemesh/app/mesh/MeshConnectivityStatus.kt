package com.cruisemesh.app.mesh

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** Health of our own relay path, as observed by the last sync attempt. */
sealed class RelayHealth {
    data class Ok(val lastSyncMs: Long) : RelayHealth()
    object Checking : RelayHealth()
    object NoInternet : RelayHealth()
    object NoConfig : RelayHealth()
    data class Failing(val lastAttemptMs: Long) : RelayHealth()
    data class Expired(val lastAttemptMs: Long) : RelayHealth()
    data class Suspended(val lastAttemptMs: Long) : RelayHealth()

    /** The relay answered but rejected our own saved family token (HTTP 401/403). */
    data class TokenRejected(val lastAttemptMs: Long) : RelayHealth()

    /**
     * CP2b: the family's hosted storage is full (HTTP 507
     * `family_quota_exceeded`). Posting fails while fetching keeps working,
     * so this is reported even when the rest of the sync pass succeeded.
     * Persistent until the family drains the backlog or it expires.
     */
    data class QuotaFull(val lastAttemptMs: Long) : RelayHealth()

    /**
     * CP2b: one queued message exceeds the per-envelope size cap (HTTP 413
     * `envelope_too_large`) and will never post as-is. Actionable locally;
     * other messages keep delivering.
     */
    data class MessageTooLarge(val lastAttemptMs: Long) : RelayHealth()

    /**
     * CP2b: the service asked us to slow down (HTTP 429 `rate_limited`).
     * Self-heals within the advertised Retry-After window; never an error
     * to act on.
     */
    data class RateLimited(val lastAttemptMs: Long) : RelayHealth()
}

enum class DirectPath { BLUETOOTH, LOCAL_WIFI }

/**
 * Process-wide observable connectivity signals, same object/StateFlow
 * pattern as [MeshRuntimeStatus]. [MeshService]
 * is the sole writer -- every event here already flows through it -- and the
 * Compose layer ([ContactReachability] callers) is the sole reader.
 */
object MeshConnectivityStatus {
    private val _nearbyPeerIds = MutableStateFlow<Set<String>>(emptySet())

    /** Distinct HELLO'd peer userIds (hex via [com.cruisemesh.app.chat.UserIdHex]), any contact or stranger. */
    val nearbyPeerIds: StateFlow<Set<String>> = _nearbyPeerIds.asStateFlow()

    private val _directPaths = MutableStateFlow<Map<String, DirectPath>>(emptyMap())
    val directPaths: StateFlow<Map<String, DirectPath>> = _directPaths.asStateFlow()

    private val _nearbyTransports = MutableStateFlow<Map<String, MeshRouterState.Transport>>(emptyMap())

    /**
     * hex userId -> the live transport a send to that peer would take right now
     * (see [MeshRouterState.nearbyTransports]). Kept as its own observable
     * signal, separate from [nearbyPeerIds], because a LAN->BLE handoff leaves
     * the peer set unchanged (still HELLO'd, just over a different radio) --
     * only this map changes, so only observing it makes the "Nearby via
     * Wi-Fi/Bluetooth" copy flip live instead of freezing on the dead
     * transport.
     */
    val nearbyTransports: StateFlow<Map<String, MeshRouterState.Transport>> = _nearbyTransports.asStateFlow()

    private val _relay = MutableStateFlow<RelayHealth>(RelayHealth.NoConfig)
    val relay: StateFlow<RelayHealth> = _relay.asStateFlow()

    private val _pushHealthy = MutableStateFlow(false)

    /**
     * `RelayPushClient`'s WS push connection state, mirrored here so the
     * Compose layer can feed [ContactReachability.selfRelayHealthy]'s
     * `pushHealthy` parameter -- battery work backs the relay poll off to a
     * 900s safety net while push is healthy, so relay-health freshness can
     * no longer rely on lastSyncMs alone (see that function's doc).
     */
    val pushHealthy: StateFlow<Boolean> = _pushHealthy.asStateFlow()

    private val _staleRelayContacts = MutableStateFlow<Set<String>>(emptySet())

    /**
     * hex userIds whose friend-card relay endpoint has been written off after
     * authoritatively rejecting us (core `contact_relay_health`).
     *
     * Distinct from [relay], which is our OWN Shore Pass's health -- both can
     * be true at once ("my pass is fine, but their card points at a host that
     * no longer knows them"). Observable so a chat's route row can say it
     * live, instead of a person discovering it from logcat as happened in the
     * field.
     */
    val staleRelayContacts: StateFlow<Set<String>> = _staleRelayContacts.asStateFlow()

    private val _contactLastSeen = MutableStateFlow<Map<String, Long>>(emptyMap())

    /** hex userId -> epoch ms we last had evidence the contact's device was alive. */
    val contactLastSeen: StateFlow<Map<String, Long>> = _contactLastSeen.asStateFlow()

    private val _presenceLastSeen = MutableStateFlow<Map<String, Long>>(emptyMap())

    /** hex userId -> epoch ms inferred from relay presence, used for ONLINE_RELAY. */
    val presenceLastSeen: StateFlow<Map<String, Long>> = _presenceLastSeen.asStateFlow()

    fun refreshNearbyRoutes() {
        val transports = MeshRouter.nearbyTransports()
        val paths = transports.mapValues { (_, transport) ->
            if (transport == MeshRouterState.Transport.LAN) {
                DirectPath.LOCAL_WIFI
            } else {
                DirectPath.BLUETOOTH
            }
        }
        _nearbyTransports.value = transports
        _directPaths.value = paths
        _nearbyPeerIds.value = transports.keys
    }

    fun setRelayHealth(health: RelayHealth) {
        _relay.value = health
    }

    /** Replaces the whole set each sync pass -- a repaired card must clear as promptly as a broken one appears. */
    fun setStaleRelayContacts(userIdHexes: Set<String>) {
        _staleRelayContacts.value = userIdHexes
    }

    /** [MeshService] calls this from [com.cruisemesh.app.relay.RelayPushClient]'s health-change callback. */
    fun setPushHealthy(healthy: Boolean) {
        _pushHealthy.value = healthy
    }

    /**
     * Records [seenAtMs] for [userIdHex], keeping the max if we already had a
     * fresher one. FA5: [MeshService] calls this from multiple concurrent
     * receive-path threads (see [InboundEnvelopeAdmission]'s KDoc), so this
     * must be a single atomic read-modify-write rather than a separate
     * `.value` read followed by a `.value` write -- [MutableStateFlow.update]
     * retries its lambda against the current value on a concurrent writer
     * race instead of silently dropping one side's update.
     */
    fun mergeLastSeen(userIdHex: String, seenAtMs: Long) {
        _contactLastSeen.update { current ->
            if (seenAtMs > (current[userIdHex] ?: 0L)) current + (userIdHex to seenAtMs) else current
        }
    }

    /** Records relay-presence freshness for [userIdHex], keeping the freshest timestamp -- same atomicity note as [mergeLastSeen]. */
    fun mergePresenceLastSeen(userIdHex: String, seenAtMs: Long) {
        _presenceLastSeen.update { current ->
            if (seenAtMs > (current[userIdHex] ?: 0L)) current + (userIdHex to seenAtMs) else current
        }
    }

    /** Mesh service stopped: every signal above is stale, so drop it all rather than show it frozen. */
    fun clear() {
        _nearbyPeerIds.value = emptySet()
        _directPaths.value = emptyMap()
        _nearbyTransports.value = emptyMap()
        _relay.value = RelayHealth.NoConfig
        _staleRelayContacts.value = emptySet()
        _contactLastSeen.value = emptyMap()
        _presenceLastSeen.value = emptyMap()
        _pushHealthy.value = false
    }
}
