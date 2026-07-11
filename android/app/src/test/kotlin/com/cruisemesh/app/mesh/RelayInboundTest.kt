package com.cruisemesh.app.mesh

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RelayInboundTest {
    @Test
    fun `consumed envelopes are acked`() {
        assertTrue(shouldAck(InboundDisposition.CONSUMED))
    }

    @Test
    fun `expired envelopes are acked`() {
        assertTrue(shouldAck(InboundDisposition.EXPIRED))
    }

    @Test
    fun `carried (proxied) envelopes are never acked`() {
        // The relay copy of a proxied 1:1 envelope must survive so the real
        // recipient (or another proxy) can still fetch it -- see
        // MeshService.pollRelayMailbox's doc for the bridging scenario this
        // protects.
        assertFalse(shouldAck(InboundDisposition.CARRIED))
    }

    @Test
    fun `already-seen envelopes are left alone`() {
        assertFalse(shouldAck(InboundDisposition.SEEN))
    }
}
