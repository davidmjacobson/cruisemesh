package com.cruisemesh.app.mesh

/**
 * Tracks whether the BLE mesh should stay up or pause itself for Bluetooth
 * Classic audio coexistence (HANDOFF.md blocking item #4).
 *
 * Policy: any connected A2DP device counts as "Bluetooth audio in use", so
 * [MeshService] pauses both BLE roles entirely until that A2DP connection is
 * gone. The class is framework-free so the transition logic is unit-testable.
 */
class A2dpAudioBackoff {
    enum class Mode { ACTIVE, PAUSED_FOR_A2DP }

    private var mode: Mode? = null

    /**
     * Returns the newly desired mode when [a2dpConnected] changes it, or null
     * if the desired mode is unchanged.
     */
    fun update(a2dpConnected: Boolean): Mode? {
        val desired = if (a2dpConnected) Mode.PAUSED_FOR_A2DP else Mode.ACTIVE
        if (mode == desired) return null
        mode = desired
        return desired
    }
}
