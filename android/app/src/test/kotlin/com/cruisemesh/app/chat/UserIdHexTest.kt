package com.cruisemesh.app.chat

import kotlin.random.Random
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class UserIdHexTest {

    @Test
    fun `encodes bytes as lowercase two-digit hex`() {
        val bytes = byteArrayOf(0x00, 0x0A, 0xFF.toByte(), 0x7B)
        assertEquals("000aff7b", UserIdHex.encode(bytes))
    }

    @Test
    fun `decode is the inverse of encode`() {
        val bytes = byteArrayOf(0x00, 0x0A, 0xFF.toByte(), 0x7B)
        assertArrayEquals(bytes, UserIdHex.decode(UserIdHex.encode(bytes)))
    }

    @Test
    fun `round trips a random 16-byte UserID`() {
        val userId = Random.nextBytes(16)
        assertArrayEquals(userId, UserIdHex.decode(UserIdHex.encode(userId)))
    }

    @Test
    fun `empty byte array round trips to empty string`() {
        assertEquals("", UserIdHex.encode(ByteArray(0)))
        assertArrayEquals(ByteArray(0), UserIdHex.decode(""))
    }

    @Test(expected = IllegalArgumentException::class)
    fun `decode rejects odd-length hex strings`() {
        UserIdHex.decode("abc")
    }
}
