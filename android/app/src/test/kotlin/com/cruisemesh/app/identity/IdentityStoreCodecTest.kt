package com.cruisemesh.app.identity

import kotlin.random.Random
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import uniffi.cruisemesh_core.Identity

class IdentityStoreCodecTest {

    private fun randomIdentity() = Identity(
        userId = Random.nextBytes(16),
        signPk = Random.nextBytes(32),
        signSk = Random.nextBytes(32),
        agreePk = Random.nextBytes(32),
        agreeSk = Random.nextBytes(32),
    )

    @Test
    fun `encode then decode round trips every field`() {
        val identity = randomIdentity()
        val decoded = decodeIdentity(encodeIdentity(identity))

        assertArrayEquals(identity.userId, decoded.userId)
        assertArrayEquals(identity.signPk, decoded.signPk)
        assertArrayEquals(identity.signSk, decoded.signSk)
        assertArrayEquals(identity.agreePk, decoded.agreePk)
        assertArrayEquals(identity.agreeSk, decoded.agreeSk)
    }

    @Test
    fun `encoded blob is exactly the sum of field sizes`() {
        val encoded = encodeIdentity(randomIdentity())
        assertEquals(144, encoded.size)
    }

    @Test
    fun `decode rejects a truncated blob rather than misreading fields`() {
        val truncated = encodeIdentity(randomIdentity()).copyOfRange(0, 100)
        assertThrows(IllegalArgumentException::class.java) { decodeIdentity(truncated) }
    }
}
