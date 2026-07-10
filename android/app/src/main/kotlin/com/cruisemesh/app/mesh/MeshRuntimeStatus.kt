package com.cruisemesh.app.mesh

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

enum class MeshRuntimeState(val label: String) {
    STOPPED("Mesh stopped"),
    STARTING("Mesh starting…"),
    ACTIVE("Mesh running"),

    /**
     * The service is up and *wants* to run, but Bluetooth is off so its BLE
     * roles (scan/advertise/GATT) can't actually carry anything. Previously the
     * status stayed [ACTIVE] in this case, so the pill said "Mesh running" while
     * the device was in fact deaf to the mesh -- exactly the state a user lands
     * in after toggling Bluetooth off and back on. Reporting it honestly lets
     * the UI say "paused" and prompt turning Bluetooth back on.
     */
    NO_BLUETOOTH("Mesh paused — Bluetooth off"),
}

/**
 * Process-wide mesh-service status for the app UI.
 *
 * The old identity screen held its own local "Mesh starting…" string and
 * never heard back from [MeshService], so the label could get stuck forever
 * even when the service was already running. This keeps one observable runtime
 * truth that both the activity and service can touch.
 *
 * [bluetoothAudioConnected] is a separate axis from [state]: as of 2026-07-09
 * the mesh no longer pauses when Bluetooth (A2DP) audio is connected -- the
 * relaxed low-power radio settings are the coexistence mitigation instead --
 * so the mesh stays [MeshRuntimeState.ACTIVE] while audio plays. This flag
 * just lets the UI show an informational banner so a user knows audio and the
 * mesh are sharing the radio (and to watch for audio glitches).
 */
object MeshRuntimeStatus {
    private val _state = MutableStateFlow(MeshRuntimeState.STOPPED)

    val state: StateFlow<MeshRuntimeState> = _state.asStateFlow()

    private val _bluetoothAudioConnected = MutableStateFlow(false)

    val bluetoothAudioConnected: StateFlow<Boolean> = _bluetoothAudioConnected.asStateFlow()

    fun markStarting() {
        _state.value = MeshRuntimeState.STARTING
    }

    fun markActive() {
        _state.value = MeshRuntimeState.ACTIVE
    }

    fun markNoBluetooth() {
        _state.value = MeshRuntimeState.NO_BLUETOOTH
    }

    fun markStopped() {
        _state.value = MeshRuntimeState.STOPPED
        _bluetoothAudioConnected.value = false
    }

    fun setBluetoothAudioConnected(connected: Boolean) {
        _bluetoothAudioConnected.value = connected
    }
}
