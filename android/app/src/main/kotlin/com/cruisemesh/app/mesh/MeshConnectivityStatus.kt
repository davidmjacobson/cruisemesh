package com.cruisemesh.app.mesh

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** Health of our own relay path, as observed by the last sync attempt. */
sealed class RelayHealth {
    data class Ok(val lastSyncMs: Long) : RelayHealth()
    object NoInternet : RelayHealth()
    object NoConfig : RelayHealth()
    data class Failing(val lastAttemptMs: Long) : RelayHealth()

    /** The relay answered but rejected our own saved family token (HTTP 401/403). */
    data class TokenRejected(val lastAttemptMs: Long) : RelayHealth()
}

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

    private val _contactLastSeen = MutableStateFlow<Map<String, Long>>(emptyMap())

    /** hex userId -> epoch ms we last had evidence the contact's device was alive. */
    val contactLastSeen: StateFlow<Map<String, Long>> = _contactLastSeen.asStateFlow()

    private val _presenceLastSeen = MutableStateFlow<Map<String, Long>>(emptyMap())

    /** hex userId -> epoch ms inferred from relay presence, used for ONLINE_RELAY. */
    val presenceLastSeen: StateFlow<Map<String, Long>> = _presenceLastSeen.asStateFlow()

    fun setNearbyPeers(peers: Set<String>) {
        _nearbyPeerIds.value = peers
    }

    fun setRelayHealth(health: RelayHealth) {
        _relay.value = health
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
        _relay.value = RelayHealth.NoConfig
        _contactLastSeen.value = emptyMap()
        _presenceLastSeen.value = emptyMap()
        _pushHealthy.value = false
    }
}
