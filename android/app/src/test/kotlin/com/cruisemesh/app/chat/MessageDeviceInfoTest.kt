package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.ContactDeviceState

/**
 * §8's rule, tested from the side that matters: the ticks are any-device and
 * the detail is behind a long press, and neither is allowed to say something a
 * core fact does not support.
 *
 * The case worth staring at is the one-device contact. Saying "any one of their
 * 1 devices got it" is technically true and is exactly the leak §2's first goal
 * forbids — a person messaging a person should not be able to count their
 * phones.
 */
class MessageDeviceInfoTest {

    private fun id(byte: Int) = ByteArray(16) { byte.toByte() }

    @Test
    fun `a received message names which of their devices sent it`() {
        val label = deviceLabelFor(id(2), listOf(id(1), id(2)), ContactDeviceState.ACTIVE)

        assertEquals(DeviceLabel.Numbered(2), label)
        assertEquals(
            listOf(DeviceInfoLine.SentFrom(DeviceLabel.Numbered(2))),
            messageDeviceInfoLines(isOwn = false, label = label, contactDeviceCount = 2),
        )
    }

    @Test
    fun `a message from a device they have since removed says so`() {
        val label = deviceLabelFor(id(9), listOf(id(1)), ContactDeviceState.REVOKED)

        assertEquals(DeviceLabel.Removed, label)
        assertEquals(
            listOf(DeviceInfoLine.SentFrom(DeviceLabel.Removed)),
            messageDeviceInfoLines(isOwn = false, label = label, contactDeviceCount = 1),
        )
    }

    @Test
    fun `a legacy peer that has never sent a device list gets one honest line`() {
        val label = deviceLabelFor(ByteArray(0), emptyList(), ContactDeviceState.UNKNOWN)

        assertEquals(DeviceLabel.Unknown, label)
        assertEquals(
            listOf(DeviceInfoLine.NoDeviceDetail),
            messageDeviceInfoLines(isOwn = false, label = label, contactDeviceCount = 0),
        )
    }

    @Test
    fun `an unknown device on a contact we do know about adds nothing`() {
        // We hold their list and this id is not on it and is not tombstoned:
        // there is nothing true to say, so nothing is said.
        val label = deviceLabelFor(id(7), listOf(id(1), id(2)), ContactDeviceState.UNKNOWN)

        assertEquals(DeviceLabel.Unknown, label)
        assertTrue(messageDeviceInfoLines(false, label, contactDeviceCount = 2).isEmpty())
    }

    @Test
    fun `a sent message never counts a one-device contact's devices`() {
        assertTrue(
            messageDeviceInfoLines(
                isOwn = true,
                label = DeviceLabel.Unknown,
                contactDeviceCount = 1,
            ).isEmpty(),
        )
        assertTrue(
            messageDeviceInfoLines(
                isOwn = true,
                label = DeviceLabel.Unknown,
                contactDeviceCount = 0,
            ).isEmpty(),
        )
    }

    @Test
    fun `a sent message to a multi-device contact says the ticks mean any of them`() {
        assertEquals(
            listOf(DeviceInfoLine.AddressedTo(3)),
            messageDeviceInfoLines(isOwn = true, label = DeviceLabel.Unknown, contactDeviceCount = 3),
        )
    }
}
