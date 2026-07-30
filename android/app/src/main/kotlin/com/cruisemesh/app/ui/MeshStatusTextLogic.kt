package com.cruisemesh.app.ui

import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth

/** Which semantic dot color the mesh status pill should show; see [MeshStatusTextLogic.build]. */
enum class MeshStatusDotColor { GREEN, BLUE, AMBER, NEUTRAL }

data class MeshStatusPillStatus(val text: String, val dot: MeshStatusDotColor?)

/**
 * Pure builder for the mesh status pill: text
 * composed from three axes (mesh runtime state x nearby peer count x relay
 * health), kept out of [MeshStatusPill] itself so it's unit-testable without
 * a Compose host, same pattern as [ChatListLogic].
 */
object MeshStatusTextLogic {
    fun build(
        runtimeState: MeshRuntimeState,
        nearbyCount: Int,
        relayHealth: RelayHealth,
    ): MeshStatusPillStatus {
        if (runtimeState != MeshRuntimeState.ACTIVE) {
            return MeshStatusPillStatus(runtimeState.label, MeshStatusDotColor.NEUTRAL)
        }
        val relaySuffix = when (relayHealth) {
            is RelayHealth.Ok -> "Cruise Pass ✓"
            RelayHealth.Checking -> "checking Cruise Pass"
            RelayHealth.NoInternet -> "no internet"
            RelayHealth.NoConfig -> "no Cruise Pass set up"
            is RelayHealth.Failing -> "Cruise Pass unreachable"
            is RelayHealth.Expired -> "Cruise Pass expired"
            is RelayHealth.Suspended -> "Cruise Pass suspended"
            is RelayHealth.TokenRejected -> "Cruise Pass token rejected"
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
