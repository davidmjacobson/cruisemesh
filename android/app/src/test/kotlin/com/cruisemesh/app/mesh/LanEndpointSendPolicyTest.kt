package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Frame

class LanEndpointSendPolicyTest {
    private val contact = Contact(
        userId = ByteArray(16) { 1 },
        name = "Peer",
        signPk = ByteArray(32) { 2 },
        agreePk = ByteArray(32) { 3 },
        relayUrl = null,
        relayToken = null,
    )
    private val hint = Frame.LanEndpoint(
        instanceToken = byteArrayOf(4, 5, 6),
        host = "10.0.0.7",
        port = 45_892.toUShort(),
    )

    @Test
    fun `authenticated contact selects exactly its current self endpoint`() {
        val selected = authenticatedLanEndpointHint(contact, hint, "network-a")

        assertSame(contact, selected?.contact)
        assertSame(hint, selected?.hint)
        assertEquals("network-a", selected?.networkId)
    }

    @Test
    fun `non-contact or incomplete LAN state selects no eager hint`() {
        assertNull(authenticatedLanEndpointHint(null, hint, "network-a"))
        assertNull(authenticatedLanEndpointHint(contact, null, "network-a"))
        assertNull(authenticatedLanEndpointHint(contact, hint, null))
    }

    @Test
    fun `same signature is claimed at most once within five minutes`() {
        val signature = lanEndpointSignature(
            networkId = "network-a",
            host = hint.host,
            port = hint.port.toInt(),
            instanceToken = hint.instanceToken,
        )
        val sentAt = 1_000L
        val record = lanEndpointSendRecord(signature, sentAt)

        assertTrue(shouldClaimLanEndpointSend(null, signature, sentAt))
        assertFalse(shouldClaimLanEndpointSend(record, signature, sentAt + 5 * 60_000L - 1))
        assertTrue(shouldClaimLanEndpointSend(record, signature, sentAt + 5 * 60_000L))
    }

    @Test
    fun `changed endpoint signature can be claimed immediately`() {
        val oldSignature = lanEndpointSignature("network-a", "10.0.0.7", 45_892, byteArrayOf(1))
        val newSignature = lanEndpointSignature("network-a", "10.0.0.8", 45_892, byteArrayOf(1))

        assertTrue(
            shouldClaimLanEndpointSend(
                lanEndpointSendRecord(oldSignature, 1_000),
                newSignature,
                1_001,
            ),
        )
    }
}
