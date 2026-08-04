package com.cruisemesh.app.ui

import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth

/** Which semantic dot color the mesh status pill should show; see [MeshStatusTextLogic.build]. */
enum class MeshStatusDotColor { GREEN, BLUE, AMBER, NEUTRAL }

data class MeshStatusPillStatus(val text: String, val dot: MeshStatusDotColor?)

enum class InternetDeliveryService(val displayName: String) {
    CRUISE_PASS("Shore Pass"),
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
 */
object MeshStatusTextLogic {
    fun build(
        runtimeState: MeshRuntimeState,
        nearbyCount: Int,
        relayHealth: RelayHealth,
        internetDeliveryService: InternetDeliveryService?,
    ): MeshStatusPillStatus {
        if (runtimeState != MeshRuntimeState.ACTIVE) {
            return MeshStatusPillStatus(runtimeState.label, MeshStatusDotColor.NEUTRAL)
        }
        // Never set anything up: nearby delivery is the free default and works
        // fine, so say nothing about internet delivery rather than reporting
        // the absence of an optional extra as a fault. Amber here would nag
        // every person who has not bought a pass, every time they open the
        // app, about a thing they did not ask for -- and would teach them to
        // ignore the dot when it finally does mean something.
        if (internetDeliveryService == null && relayHealth == RelayHealth.NoConfig) {
            return if (nearbyCount > 0) {
                MeshStatusPillStatus("Mesh on · $nearbyCount nearby", MeshStatusDotColor.GREEN)
            } else {
                MeshStatusPillStatus("Mesh on", MeshStatusDotColor.NEUTRAL)
            }
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
                if (internetDeliveryService == InternetDeliveryService.CRUISE_PASS) {
                    "Shore Pass expired"
                } else {
                    "$serviceName pass expired"
                }
            is RelayHealth.Suspended ->
                if (internetDeliveryService == InternetDeliveryService.CRUISE_PASS) {
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
        val dot = when {
            nearbyCount > 0 -> MeshStatusDotColor.GREEN
            relayHealth is RelayHealth.Ok -> MeshStatusDotColor.BLUE
            else -> MeshStatusDotColor.AMBER
        }
        return MeshStatusPillStatus(text, dot)
    }
}
