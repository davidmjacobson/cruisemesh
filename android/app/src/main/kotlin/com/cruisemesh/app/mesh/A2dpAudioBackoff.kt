package com.cruisemesh.app.mesh

/**
 * Edge detector for "is an A2DP device connected", so [MeshService] acts on the
 * transition once rather than on every broadcast. The class is framework-free
 * so the transition logic is unit-testable.
 *
 * It used to pause both BLE roles, and [Mode.PAUSED_FOR_A2DP] is a leftover of
 * that name. It no longer pauses anything: messaging was dead on a phone
 * whenever earbuds were connected, so 2026-07-09 kept the mesh running and made
 * the relaxed low-power scan/advertise settings the coexistence mitigation. The
 * connection state now only drives an informational banner and notification.
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
