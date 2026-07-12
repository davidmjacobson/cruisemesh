package com.cruisemesh.app.identity.backup

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class BackupCodecTest {

    private fun identity(fill: Byte = 7) = ByteArray(BackupFormat.IDENTITY_LEN) { fill }

    @Test
    fun `header round-trips through encode then decode`() {
        val kdfParams = BackupCodec.pbkdf2Params(600_000)
        val salt = ByteArray(BackupFormat.SALT_LEN) { it.toByte() }
        val nonce = ByteArray(BackupFormat.NONCE_LEN) { (it + 100).toByte() }
        val header = BackupCodec.encodeHeader(BackupFormat.KdfId.PBKDF2_HMAC_SHA256, kdfParams, salt, nonce)
        // Append a minimal ciphertext+tag so length checks pass.
        val file = header + ByteArray(BackupFormat.GCM_TAG_LEN + 5) { 1 }

        val decoded = BackupCodec.decodeHeader(file)
        assertEquals(BackupFormat.KdfId.PBKDF2_HMAC_SHA256, decoded.kdfId)
        assertEquals(600_000, decoded.pbkdf2Iterations)
        assertArrayEquals(salt, decoded.salt)
        assertArrayEquals(nonce, decoded.nonce)
        assertArrayEquals(header, decoded.aad)
        assertEquals(BackupFormat.GCM_TAG_LEN + 5, decoded.ciphertext.size)
    }

    @Test
    fun `encoded header is exactly HEADER_LEN bytes`() {
        val header = BackupCodec.encodeHeader(
            BackupFormat.KdfId.PBKDF2_HMAC_SHA256,
            BackupCodec.pbkdf2Params(1),
            ByteArray(BackupFormat.SALT_LEN),
            ByteArray(BackupFormat.NONCE_LEN),
        )
        assertEquals(BackupFormat.HEADER_LEN, header.size)
    }

    @Test
    fun `bad magic is rejected`() {
        val file = ByteArray(BackupFormat.HEADER_LEN + BackupFormat.GCM_TAG_LEN) { 0xAB.toByte() }
        assertThrows(BackupException.BadMagic::class.java) { BackupCodec.decodeHeader(file) }
    }

    @Test
    fun `unsupported outer version is rejected`() {
        val header = BackupCodec.encodeHeader(
            BackupFormat.KdfId.PBKDF2_HMAC_SHA256,
            BackupCodec.pbkdf2Params(1),
            ByteArray(BackupFormat.SALT_LEN),
            ByteArray(BackupFormat.NONCE_LEN),
        )
        header[BackupFormat.MAGIC.size] = 99 // corrupt the version byte
        val file = header + ByteArray(BackupFormat.GCM_TAG_LEN)
        val ex = assertThrows(BackupException.UnsupportedVersion::class.java) { BackupCodec.decodeHeader(file) }
        assertEquals(99, ex.version)
    }

    @Test
    fun `too-short file is truncated`() {
        assertThrows(BackupException.Truncated::class.java) {
            BackupCodec.decodeHeader(ByteArray(BackupFormat.HEADER_LEN)) // no room for tag
        }
    }

    @Test
    fun `inner payload round-trips`() {
        val payload = BackupPayload(
            identity = identity(3),
            sqlite = ByteArray(1000) { (it % 256).toByte() },
            srcVersionCode = 42,
            createdAtMs = 1_700_000_000_000L,
        )
        val decoded = BackupCodec.decodeInner(BackupCodec.encodeInner(payload))
        assertEquals(payload, decoded)
    }

    @Test
    fun `inner payload with empty sqlite round-trips`() {
        val payload = BackupPayload(identity(1), ByteArray(0), 1, 0L)
        assertEquals(payload, BackupCodec.decodeInner(BackupCodec.encodeInner(payload)))
    }

    @Test
    fun `encodeInner rejects a wrong-sized identity`() {
        val payload = BackupPayload(ByteArray(10), ByteArray(0), 1, 0L)
        assertThrows(IllegalArgumentException::class.java) { BackupCodec.encodeInner(payload) }
    }

    @Test
    fun `truncated inner plaintext is a typed Truncated error`() {
        val good = BackupCodec.encodeInner(BackupPayload(identity(), ByteArray(500) { 9 }, 1, 1L))
        val chopped = good.copyOfRange(0, good.size - 100) // lose part of the sqlite blob
        assertThrows(BackupException.Truncated::class.java) { BackupCodec.decodeInner(chopped) }
    }

    @Test
    fun `inner plaintext claiming more sqlite than present is Truncated`() {
        // Hand-build an inner blob whose declared sqlite length overruns the buffer.
        val payload = BackupPayload(identity(), ByteArray(4) { 1 }, 1, 1L)
        val encoded = BackupCodec.encodeInner(payload)
        // The sqlite length field sits right after inner_version(1)+srcVersion(4)+createdAt(8)+idLen(2)+identity(144).
        val lenOffset = 1 + 4 + 8 + 2 + BackupFormat.IDENTITY_LEN
        encoded[lenOffset] = 0x7F // blow the length up to a huge positive number
        assertThrows(BackupException.Truncated::class.java) { BackupCodec.decodeInner(encoded) }
    }
}
