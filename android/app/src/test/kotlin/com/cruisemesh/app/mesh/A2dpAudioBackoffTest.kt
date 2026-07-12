package com.cruisemesh.app.mesh

import android.bluetooth.BluetoothProfile
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class A2dpAudioBackoffTest {

    @Test
    fun `first disconnected snapshot keeps the mesh active`() {
        val backoff = A2dpAudioBackoff()

        assertEquals(A2dpAudioBackoff.Mode.ACTIVE, backoff.update(a2dpConnected = false))
    }

    @Test
    fun `first connected snapshot pauses the mesh for audio`() {
        val backoff = A2dpAudioBackoff()

        assertEquals(A2dpAudioBackoff.Mode.PAUSED_FOR_A2DP, backoff.update(a2dpConnected = true))
    }

    @Test
    fun `repeating the same A2DP state is a no-op`() {
        val backoff = A2dpAudioBackoff()
        backoff.update(a2dpConnected = true)

        assertNull(backoff.update(a2dpConnected = true))
    }

    @Test
    fun `disconnecting A2DP after a pause resumes the mesh`() {
        val backoff = A2dpAudioBackoff()
        backoff.update(a2dpConnected = true)

        assertEquals(A2dpAudioBackoff.Mode.ACTIVE, backoff.update(a2dpConnected = false))
    }

    @Test
    fun `broadcast connected and disconnected states are authoritative`() {
        assertEquals(true, bluetoothAudioConnectedFromProfileState(BluetoothProfile.STATE_CONNECTED))
        assertEquals(false, bluetoothAudioConnectedFromProfileState(BluetoothProfile.STATE_DISCONNECTED))
    }

    @Test
    fun `transitional broadcast states fall back to profile query`() {
        assertNull(bluetoothAudioConnectedFromProfileState(BluetoothProfile.STATE_CONNECTING))
        assertNull(bluetoothAudioConnectedFromProfileState(BluetoothProfile.STATE_DISCONNECTING))
        assertNull(bluetoothAudioConnectedFromProfileState(-1))
    }
}
