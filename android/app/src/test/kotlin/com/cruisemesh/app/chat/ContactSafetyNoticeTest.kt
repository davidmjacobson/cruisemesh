package com.cruisemesh.app.chat

import com.cruisemesh.app.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.ContactSafetyFact
import uniffi.cruisemesh_core.ContactSafetyReason

/**
 * §10.4's surface reads one fact and shows one line. What is worth pinning is
 * which fact it picks when several are outstanding, and that only the fork
 * offers the "I checked" action — the other two reasons describe something that
 * happened rather than a state a person can clear.
 */
class ContactSafetyNoticeTest {

    private fun id(byte: Int) = ByteArray(16) { byte.toByte() }

    private fun fact(
        person: ByteArray,
        reason: ContactSafetyReason = ContactSafetyReason.DEVICE_REVOKED,
        observedSeq: ULong,
        acknowledged: Boolean = false,
    ) = ContactSafetyFact(
        personUserId = person,
        reason = reason,
        deviceIds = emptyList(),
        recoveryEpoch = 0uL,
        seq = 1uL,
        observedSeq = observedSeq,
        acknowledged = acknowledged,
    )

    @Test
    fun `the newest unacknowledged fact for this contact wins`() {
        val alice = id(1)
        val facts = listOf(
            fact(alice, observedSeq = 3uL),
            fact(alice, reason = ContactSafetyReason.ROSTER_FORKED, observedSeq = 9uL),
            fact(id(2), observedSeq = 12uL),
        )

        val chosen = latestSafetyFact(facts, alice)

        assertEquals(9uL, chosen?.observedSeq)
        assertEquals(ContactSafetyReason.ROSTER_FORKED, chosen?.reason)
    }

    @Test
    fun `an acknowledged fact is not shown again`() {
        val alice = id(1)
        val facts = listOf(fact(alice, observedSeq = 4uL, acknowledged = true))

        assertNull(latestSafetyFact(facts, alice))
    }

    @Test
    fun `another contact's facts never surface in this chat`() {
        assertNull(latestSafetyFact(listOf(fact(id(2), observedSeq = 1uL)), id(1)))
    }

    @Test
    fun `only a fork offers the out-of-band check`() {
        assertTrue(offersOutOfBandCheck(ContactSafetyReason.ROSTER_FORKED))
        assertFalse(offersOutOfBandCheck(ContactSafetyReason.DEVICE_REVOKED))
        assertFalse(offersOutOfBandCheck(ContactSafetyReason.IDENTITY_RECOVERED))
    }

    @Test
    fun `every reason core can raise has a line of its own`() {
        val copies = ContactSafetyReason.values().map { contactSafetyCopy(it) }

        assertEquals(copies.size, copies.toSet().size)
        assertEquals(R.string.ui_contact_removed_a_device, contactSafetyCopy(ContactSafetyReason.DEVICE_REVOKED))
        assertEquals(R.string.ui_contact_set_up_again, contactSafetyCopy(ContactSafetyReason.IDENTITY_RECOVERED))
        assertEquals(
            R.string.ui_contact_devices_dont_add_up,
            contactSafetyCopy(ContactSafetyReason.ROSTER_FORKED),
        )
    }
}
