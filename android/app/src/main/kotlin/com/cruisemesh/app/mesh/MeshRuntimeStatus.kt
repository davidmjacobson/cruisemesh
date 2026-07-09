package com.cruisemesh.app.mesh

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

enum class MeshRuntimeState(val label: String) {
    STOPPED("Mesh stopped"),
    STARTING("Mesh starting…"),
    ACTIVE("Mesh running"),
    PAUSED_FOR_BLUETOOTH_AUDIO("Mesh paused for Bluetooth audio"),
}

/**
 * Process-wide mesh-service status for the app UI.
 *
 * The old identity screen held its own local "Mesh starting…" string and
 * never heard back from [MeshService], so the label could get stuck forever
 * even when the service was already running or deliberately paused for A2DP.
 * This keeps one observable runtime truth that both the activity and service
 * can touch.
 */
object MeshRuntimeStatus {
    private val _state = MutableStateFlow(MeshRuntimeState.STOPPED)

    val state: StateFlow<MeshRuntimeState> = _state.asStateFlow()

    fun markStarting() {
        _state.value = MeshRuntimeState.STARTING
    }

    fun markActive() {
        _state.value = MeshRuntimeState.ACTIVE
    }

    fun markPausedForBluetoothAudio() {
        _state.value = MeshRuntimeState.PAUSED_FOR_BLUETOOTH_AUDIO
    }

    fun markStopped() {
        _state.value = MeshRuntimeState.STOPPED
    }
}
