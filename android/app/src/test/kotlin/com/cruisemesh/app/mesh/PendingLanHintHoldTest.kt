package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.Frame

private fun hint(token: Byte = 1, host: String = "192.168.1.5", port: Int = 5000) =
    Frame.LanEndpoint(instanceToken = byteArrayOf(token), host = host, port = port.toUShort())

class PendingLanHintHoldTest {

    @Test
    fun `a stashed hint can be taken exactly once`() {
        // Frame.LanEndpoint's generated equals() compares instanceToken by
        // ByteArray reference, not content, so assert on the fields
        // individually rather than object equality between two separately
        // constructed instances.
        val hold = PendingLanHintHold()
        hold.stash("AA:BB", hint())
        val taken = hold.take("AA:BB")
        assertEquals("192.168.1.5", taken?.host)
        assertEquals(5000, taken?.port?.toInt())
        assertNull(hold.take("AA:BB"))
    }

    @Test
    fun `an address with no stashed hint returns null`() {
        val hold = PendingLanHintHold()
        assertNull(hold.take("AA:BB"))
    }

    @Test
    fun `re-stashing the same address replaces the earlier hint`() {
        val hold = PendingLanHintHold()
        hold.stash("AA:BB", hint(host = "10.0.0.1"))
        hold.stash("AA:BB", hint(host = "10.0.0.2"))
        assertEquals(1, hold.size())
        assertEquals("10.0.0.2", hold.take("AA:BB")?.host)
    }

    @Test
    fun `clear discards a held hint for one address without affecting others`() {
        val hold = PendingLanHintHold()
        hold.stash("AA:BB", hint())
        hold.stash("CC:DD", hint())
        hold.clear("AA:BB")
        assertNull(hold.take("AA:BB"))
        assertEquals("192.168.1.5", hold.take("CC:DD")?.host)
    }

    @Test
    fun `capacity is bounded and the oldest entry is evicted first`() {
        val hold = PendingLanHintHold(maxEntries = 2)
        hold.stash("A", hint(host = "1"))
        hold.stash("B", hint(host = "2"))
        hold.stash("C", hint(host = "3"))
        assertEquals(2, hold.size())
        assertNull(hold.take("A"))
        assertEquals("2", hold.take("B")?.host)
        assertEquals("3", hold.take("C")?.host)
    }

    @Test
    fun `re-stashing an existing address moves it to the front of the eviction order`() {
        val hold = PendingLanHintHold(maxEntries = 2)
        hold.stash("A", hint(host = "1"))
        hold.stash("B", hint(host = "2"))
        // A is refreshed -- it's now the freshest, so the next insert should
        // evict B (the actual oldest), not A.
        hold.stash("A", hint(host = "1-refreshed"))
        hold.stash("C", hint(host = "3"))
        assertNull(hold.take("B"))
        assertEquals("1-refreshed", hold.take("A")?.host)
        assertEquals("3", hold.take("C")?.host)
    }

    @Test
    fun `default max entries is 8`() {
        val hold = PendingLanHintHold()
        for (i in 0 until 10) {
            hold.stash("addr-$i", hint(host = i.toString()))
        }
        assertEquals(PendingLanHintHold.MAX_ENTRIES, hold.size())
        // The two oldest (addr-0, addr-1) should have been evicted.
        assertNull(hold.take("addr-0"))
        assertNull(hold.take("addr-1"))
        assertEquals("2", hold.take("addr-2")?.host)
    }
}
