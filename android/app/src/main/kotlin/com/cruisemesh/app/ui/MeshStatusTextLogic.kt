package com.cruisemesh.app.ui

import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth
import uniffi.cruisemesh_core.CoreConnectionHealth
import uniffi.cruisemesh_core.CoreConnectionHealthInput
import uniffi.cruisemesh_core.CoreRelayPathState
import uniffi.cruisemesh_core.coreClassifyConnectionHealth

/** Which semantic dot color the mesh status pill should show; see [MeshStatusTextLogic.build]. */
enum class MeshStatusDotColor { GREEN, BLUE, AMBER, NEUTRAL }

data class MeshStatusPillStatus(
    val text: String,
    val dot: MeshStatusDotColor?,
    /**
     * The core's verdict on this device's connection ([coreClassifyConnectionHealth]).
     *
     * Carried on the record rather than kept private so the pill's state is
     * inspectable in tests: the property that matters is that it is the *same*
     * value the Connection details health card renders, and a value nobody can
     * read is a property nobody can pin.
     */
    val health: CoreConnectionHealth,
)

enum class InternetDeliveryService(val displayName: String) {
    SHORE_PASS("Shore Pass"),
    CUSTOM_RELAY("relay"),
}

/**
 * Pure builder for the mesh status pill: text
 * composed from three axes (mesh runtime state x nearby peer count x relay
 * health), kept out of [MeshStatusPill] itself so it's unit-testable without
 * a Compose host, same pattern as [ChatListLogic].
 *
 * [InternetDeliveryService] doubles as the "is anything set up at all"
 * signal: it is null exactly when no setup card is saved. That distinction
 * matters because [RelayHealth] alone cannot express it -- a phone that has
 * never had a pass and a phone whose saved pass has not been checked yet both
 * report [RelayHealth.NoConfig] -- and the two deserve opposite treatment.
 * This is the same asymmetry `passIndicator` documents for the Settings row.
 *
 * **The severity is not decided here.** The dot comes from
 * [coreClassifyConnectionHealth] -- the same call, on the same inputs, that
 * produces the Connection details health card -- so the pill and that page
 * cannot claim different things about the same phone. Before, they could and
 * did: a phone with friends nearby and an expired pass showed a green pill
 * over a page reading `Working, with limits`, and a pass fault with nobody
 * nearby was amber on Android and something else again on iOS. The text
 * remains this file's own, because a pill is a one-line summary and the card
 * is a paragraph; what must agree is the verdict, not the wording.
 */
object MeshStatusTextLogic {
    @Suppress("LongParameterList")
    fun build(
        runtimeState: MeshRuntimeState,
        nearbyCount: Int,
        relayHealth: RelayHealth,
        internetDeliveryService: InternetDeliveryService?,
        lanListening: Boolean,
        checkingSinceMs: Long,
        nowMs: Long,
    ): MeshStatusPillStatus {
        val health = coreClassifyConnectionHealth(
            CoreConnectionHealthInput(
                runtime = ConnectionInputs.runtime(runtimeState),
                bluetooth = ConnectionInputs.bluetooth(runtimeState),
                // The pill counts *peers*, not per-radio links, and the counts
                // feed only the core's evidence record, which the pill does not
                // render. Splitting a number the pill does not have would be
                // inventing evidence to look thorough.
                bluetoothLinks = 0u,
                localWifi = ConnectionInputs.localWifi(runtimeState, lanListening),
                localWifiLinks = 0u,
                relay = ConnectionInputs.relay(relayHealth, internetDeliveryService != null),
                validatedInternet = ConnectionInputs.validatedInternet(relayHealth),
                nearbyFriendCount = nearbyCount.coerceAtLeast(0).toUInt(),
                checkingSinceMs = checkingSinceMs,
                nowMs = nowMs,
            ),
        )
        val dot = dotFor(health.state, nearbyCount, health.evidence.relay)
        if (runtimeState != MeshRuntimeState.ACTIVE) {
            return MeshStatusPillStatus(runtimeState.label, dot, health.state)
        }
        // Never set anything up: nearby delivery is the free default and works
        // fine, so say nothing about internet delivery rather than reporting
        // the absence of an optional extra as a fault. Amber here would nag
        // every person who has not bought a pass, every time they open the
        // app, about a thing they did not ask for -- and would teach them to
        // ignore the dot when it finally does mean something.
        if (internetDeliveryService == null && relayHealth == RelayHealth.NoConfig) {
            val text = if (nearbyCount > 0) {
                "Mesh on · $nearbyCount nearby"
            } else {
                "Mesh on"
            }
            return MeshStatusPillStatus(text, dot, health.state)
        }
        val serviceName = internetDeliveryService?.displayName ?: "internet delivery"
        val relaySuffix = when (relayHealth) {
            is RelayHealth.Ok -> "$serviceName ✓"
            RelayHealth.Checking -> "checking $serviceName"
            RelayHealth.NoInternet -> "no internet"
            // Only reachable with a service configured (the null case returned
            // above): a saved card the running mesh has not checked yet.
            RelayHealth.NoConfig -> "no internet delivery set up"
            is RelayHealth.Failing -> "$serviceName unreachable"
            is RelayHealth.Expired ->
                if (internetDeliveryService == InternetDeliveryService.SHORE_PASS) {
                    "Shore Pass expired"
                } else {
                    "$serviceName pass expired"
                }
            is RelayHealth.Suspended ->
                if (internetDeliveryService == InternetDeliveryService.SHORE_PASS) {
                    "Shore Pass suspended"
                } else {
                    "$serviceName pass suspended"
                }
            is RelayHealth.TokenRejected -> "$serviceName token rejected"
            is RelayHealth.QuotaFull -> "storage full"
            is RelayHealth.MessageTooLarge -> "message too large"
            is RelayHealth.RateLimited -> "syncing slowed"
        }
        val text = when {
            relayHealth == RelayHealth.NoInternet && nearbyCount == 0 -> "Mesh on · offline"
            nearbyCount > 0 -> "Mesh on · $nearbyCount nearby · $relaySuffix"
            else -> "Mesh on · $relaySuffix"
        }
        return MeshStatusPillStatus(text, dot, health.state)
    }

    /**
     * The dot, from the core's verdict.
     *
     * `Ready` still distinguishes green from blue from neutral, because those
     * three are not severities: they say which path is carrying, and a person
     * who is used to green meaning "someone is here" would lose that reading if
     * every healthy state looked alike. Everything that *is* a severity comes
     * from the core, so a degraded phone can no longer show green just because
     * a friend happens to be in the room.
     */
    private fun dotFor(
        state: CoreConnectionHealth,
        nearbyCount: Int,
        relay: CoreRelayPathState,
    ): MeshStatusDotColor = when (state) {
        // No verdict yet is not a warning. The card shows a spinner here; the
        // pill has no room for one, and a colored dot would be a claim.
        CoreConnectionHealth.CHECKING -> MeshStatusDotColor.NEUTRAL
        CoreConnectionHealth.LIMITED,
        CoreConnectionHealth.NEEDS_ATTENTION,
        -> MeshStatusDotColor.AMBER
        CoreConnectionHealth.READY -> when {
            nearbyCount > 0 -> MeshStatusDotColor.GREEN
            relay == CoreRelayPathState.CONNECTED -> MeshStatusDotColor.BLUE
            // Listening, with nobody here and nothing to report. This is the
            // ordinary state of a phone on a quiet morning and must not look
            // like a problem.
            else -> MeshStatusDotColor.NEUTRAL
        }
    }
}
