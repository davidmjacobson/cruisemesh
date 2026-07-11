package com.cruisemesh.app.mesh

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Health of our own relay path, as observed by the last sync attempt. */
sealed class RelayHealth {
    data class Ok(val lastSyncMs: Long) : RelayHealth()
    object NoInternet : RelayHealth()
    object NoConfig : RelayHealth()
    data class Failing(val lastAttemptMs: Long) : RelayHealth()
}

/**
 * Process-wide observable connectivity signals (CONNECTIVITY_INDICATOR.md
 * §4), same object/StateFlow pattern as [MeshRuntimeStatus]. [MeshService]
 * is the sole writer -- every event here already flows through it -- and the
 * Compose layer ([ContactReachability] callers) is the sole reader.
 */
object MeshConnectivityStatus {
    private val _nearbyPeerIds = MutableStateFlow<Set<String>>(emptySet())

    /** Distinct HELLO'd peer userIds (hex via [com.cruisemesh.app.chat.UserIdHex]), any contact or stranger. */
    val nearbyPeerIds: StateFlow<Set<String>> = _nearbyPeerIds.asStateFlow()

    private val _relay = MutableStateFlow<RelayHealth>(RelayHealth.NoConfig)
    val relay: StateFlow<RelayHealth> = _relay.asStateFlow()

    private val _contactLastSeen = MutableStateFlow<Map<String, Long>>(emptyMap())

    /** hex userId -> epoch ms we last had evidence the contact's device was alive. */
    val contactLastSeen: StateFlow<Map<String, Long>> = _contactLastSeen.asStateFlow()

    fun setNearbyPeers(peers: Set<String>) {
        _nearbyPeerIds.value = peers
    }

    fun setRelayHealth(health: RelayHealth) {
        _relay.value = health
    }

    /** Records [seenAtMs] for [userIdHex], keeping the max if we already had a fresher one. */
    fun mergeLastSeen(userIdHex: String, seenAtMs: Long) {
        val current = _contactLastSeen.value
        if (seenAtMs > (current[userIdHex] ?: 0L)) {
            _contactLastSeen.value = current + (userIdHex to seenAtMs)
        }
    }

    /** Mesh service stopped: every signal above is stale, so drop it all rather than show it frozen. */
    fun clear() {
        _nearbyPeerIds.value = emptySet()
        _relay.value = RelayHealth.NoConfig
        _contactLastSeen.value = emptyMap()
    }
}
