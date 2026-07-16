package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SameLanProbeTest {
    @Test
    fun `probe frame contains magic and exactly sixteen nonce bytes`() {
        val nonce = ByteArray(SameLanProbeProtocol.NONCE_SIZE) { it.toByte() }
        val frame = SameLanProbeProtocol.makeFrame(nonce)

        assertEquals(20, frame.size)
        assertTrue(SameLanProbeProtocol.isProbeFrame(frame))
        assertTrue(frame.copyOfRange(4, frame.size).contentEquals(nonce))
    }

    @Test
    fun `invalid magic and frame lengths are rejected`() {
        val frame = SameLanProbeProtocol.makeFrame(ByteArray(16))
        assertFalse(SameLanProbeProtocol.isProbeFrame(frame.copyOf(19)))
        assertFalse(SameLanProbeProtocol.isProbeFrame(frame.copyOf().also { it[0] = 'X'.code.toByte() }))
    }

    @Test
    fun `only the exact challenge is accepted as an echo`() {
        val sent = SameLanProbeProtocol.makeFrame(ByteArray(16) { it.toByte() })
        assertTrue(SameLanProbeProtocol.isExpectedEcho(sent, sent.copyOf()))
        assertFalse(
            SameLanProbeProtocol.isExpectedEcho(
                sent,
                sent.copyOf().also { it[it.lastIndex] = 99 },
            ),
        )
    }

    @Test
    fun `timeout distinguishes no peer from blocked peer`() {
        assertEquals(SameLanProbePhase.NO_PEER, terminalProbePhase(sawPeer = false, connectionFailed = false))
        assertEquals(SameLanProbePhase.BLOCKED, terminalProbePhase(sawPeer = true, connectionFailed = false))
        assertEquals(SameLanProbePhase.BLOCKED, terminalProbePhase(sawPeer = false, connectionFailed = true))
    }

    @Test
    fun `no peer retries sooner than a blocked connection`() {
        assertTrue(retryDelayMs(SameLanProbePhase.NO_PEER) < retryDelayMs(SameLanProbePhase.BLOCKED))
    }
}
