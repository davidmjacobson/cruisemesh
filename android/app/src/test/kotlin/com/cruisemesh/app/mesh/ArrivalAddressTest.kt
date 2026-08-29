package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The bug this type exists to prevent: a relay-fetched envelope was labelled
 * `"relay"` and the label was then handed to `MeshRouter.sendToAddress` as if
 * it were a link, so the delivered/read receipt for a message that arrived
 * over the internet could never go out on the spot. A label and a link are
 * different things here, and only one of them may be sent on.
 */
class ArrivalAddressTest {

    @Test
    fun `a relay arrival offers no link to send on`() {
        val source = ArrivalAddress.of(null)

        assertNull("the relay label must never be reachable as a sendable address", source.link)
        assertFalse(source.isLiveLink)
    }

    @Test
    fun `a relay arrival still reads as relay in a log line`() {
        val source = ArrivalAddress.of(null)

        assertEquals("relay", source.label)
        assertEquals("relay", "$source")
    }

    @Test
    fun `a live link arrival keeps the exact link it arrived on`() {
        val source = ArrivalAddress.of("AA:BB:CC:DD:EE:FF")

        assertEquals("AA:BB:CC:DD:EE:FF", source.link)
        assertTrue(source.isLiveLink)
        assertEquals("AA:BB:CC:DD:EE:FF", source.label)
        assertEquals("AA:BB:CC:DD:EE:FF", "$source")
    }

    @Test
    fun `a LAN arrival is a live link like any other`() {
        val source = ArrivalAddress.of("192.168.1.24:47411")

        assertEquals("192.168.1.24:47411", source.link)
        assertTrue(source.isLiveLink)
    }

    @Test
    fun `the relay constant and a null source are the same arrival`() {
        assertEquals(ArrivalAddress.relay, ArrivalAddress.of(null))
    }

    @Test
    fun `a link that happens to spell the relay label is still a link`() {
        // Nothing ever names a BLE or LAN link this, but the discriminant is
        // the presence of a source address, never the spelling of one.
        val source = ArrivalAddress.of(ArrivalAddress.RELAY_LABEL)

        assertEquals(ArrivalAddress.RELAY_LABEL, source.link)
        assertTrue(source.isLiveLink)
    }
}
