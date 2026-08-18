package com.cruisemesh.app.devicelink

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * DL-3's receive rule, pinned.
 *
 * The case that matters is the third one: a document that is perfectly valid,
 * correctly signed, and about somebody other than the person who sent it. The
 * signature chain has nothing to say about that — which is why the rule exists
 * and why it is worth a test that will fail loudly if the line is ever
 * "simplified" away while this shell still needs it.
 */
class RosterGossipTest {

    private fun id(byte: Int) = ByteArray(16) { byte.toByte() }

    @Test
    fun `a person's own device list is accepted`() {
        assertTrue(rosterGossipDescribesSender(id(1), id(1)))
    }

    @Test
    fun `a genuine document about somebody else is refused`() {
        // Not forged. Just replayed by a contact who also holds a copy -- and a
        // stale copy still vouches for a device its person has since buried.
        assertFalse(rosterGossipDescribesSender(id(2), id(1)))
    }

    @Test
    fun `an empty person id is never taken as a match`() {
        assertFalse(rosterGossipDescribesSender(ByteArray(0), ByteArray(0)))
        assertFalse(rosterGossipDescribesSender(ByteArray(0), id(1)))
    }
}
