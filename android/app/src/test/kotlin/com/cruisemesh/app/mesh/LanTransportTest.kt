package com.cruisemesh.app.mesh

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.lanDefaultTcpPort
import uniffi.cruisemesh_core.lanServiceType

class LanTransportTest {
    private fun contact(userByte: Int, agreeByte: Int) = Contact(
        userId = ByteArray(16) { userByte.toByte() },
        name = "Peer $userByte",
        signPk = ByteArray(32) { (userByte + 1).toByte() },
        agreePk = ByteArray(32) { agreeByte.toByte() },
        relayUrl = null,
        relayToken = null,
    )

    @Test
    fun `default port is a high IANA user port`() {
        assertEquals(45_892, lanDefaultTcpPort().toInt())
        assertTrue(lanDefaultTcpPort().toInt() in 1_024..49_151)
    }

    @Test
    fun `Android and Bonjour service type spelling variants match`() {
        assertTrue(sameLanServiceType("_cruisemesh._tcp"))
        assertTrue(sameLanServiceType("_cruisemesh._tcp."))
        assertEquals("_cruisemesh._tcp.", lanServiceType())
    }

    @Test
    fun `discovery tokens elect exactly one connection initiator`() {
        assertTrue(shouldInitiateLanConnection("0011", "aabb"))
        assertTrue(!shouldInitiateLanConnection("aabb", "0011"))
        assertTrue(!shouldInitiateLanConnection("aabb", "aabb"))
    }

    @Test
    fun `Noise static key resolves only an accepted contact`() {
        val alice = contact(1, 7)
        val bob = contact(2, 8)

        assertArrayEquals(
            bob.userId,
            trustedLanPeerUserId(listOf(alice, bob), bob.agreePk),
        )
        assertNull(trustedLanPeerUserId(listOf(alice, bob), ByteArray(32) { 9 }))
    }
}
