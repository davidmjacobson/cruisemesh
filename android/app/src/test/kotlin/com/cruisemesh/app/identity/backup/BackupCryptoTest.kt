package com.cruisemesh.app.identity.backup

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class BackupCryptoTest {

    // Low iteration count keeps the unit test fast; production uses the default 600k.
    private val fastIters = 1_000

    private fun samplePayload() = BackupPayload(
        identity = ByteArray(BackupFormat.IDENTITY_LEN) { (it * 3).toByte() },
        sqlite = ByteArray(4096) { (it % 251).toByte() },
        srcVersionCode = 7,
        createdAtMs = 1_720_000_000_000L,
    )

    @Test
    fun `seal then open round-trips the payload`() {
        val payload = samplePayload()
        val file = BackupCrypto.seal("hunter2 correct horse".toCharArray(), payload, iterations = fastIters)
        val restored = BackupCrypto.open("hunter2 correct horse".toCharArray(), file)
        assertEquals(payload, restored)
    }

    @Test
    fun `wrong passphrase fails with a typed error`() {
        val file = BackupCrypto.seal("right-passphrase".toCharArray(), samplePayload(), iterations = fastIters)
        assertThrows(BackupException.WrongPassphraseOrCorrupt::class.java) {
            BackupCrypto.open("wrong-passphrase".toCharArray(), file)
        }
    }

    @Test
    fun `flipping a ciphertext byte fails the tag`() {
        val file = BackupCrypto.seal("pw".toCharArray(), samplePayload(), iterations = fastIters)
        file[file.size - 1] = (file[file.size - 1].toInt() xor 0x01).toByte() // last byte is inside the GCM tag
        assertThrows(BackupException.WrongPassphraseOrCorrupt::class.java) {
            BackupCrypto.open("pw".toCharArray(), file)
        }
    }

    @Test
    fun `tampering with the header AAD fails decryption`() {
        val file = BackupCrypto.seal("pw".toCharArray(), samplePayload(), iterations = fastIters)
        // Corrupt the stored iteration count (inside the header/AAD). The KDF
        // then derives a different key AND the AAD no longer matches, so this
        // fails either on the tag or the KDF mismatch — either way, typed.
        file[8] = (file[8].toInt() xor 0x7F).toByte() // first byte of kdf_params
        assertThrows(BackupException::class.java) {
            BackupCrypto.open("pw".toCharArray(), file)
        }
    }

    @Test
    fun `two seals of the same payload differ (random salt and nonce)`() {
        val pw = "same".toCharArray()
        val payload = samplePayload()
        val a = BackupCrypto.seal(pw.copyOf(), payload, iterations = fastIters)
        val b = BackupCrypto.seal(pw.copyOf(), payload, iterations = fastIters)
        assertFalse("distinct salt/nonce must yield distinct ciphertext", a.contentEquals(b))
        // Both still open to the same payload.
        assertEquals(payload, BackupCrypto.open(pw.copyOf(), a))
        assertEquals(payload, BackupCrypto.open(pw.copyOf(), b))
    }

    @Test
    fun `deriveKey is deterministic and 256-bit`() {
        val salt = ByteArray(BackupFormat.SALT_LEN) { it.toByte() }
        val k1 = BackupCrypto.deriveKey("pw".toCharArray(), salt, fastIters).encoded
        val k2 = BackupCrypto.deriveKey("pw".toCharArray(), salt, fastIters).encoded
        assertArrayEquals(k1, k2)
        assertEquals(32, k1.size)
    }

    @Test
    fun `deriveKey diverges on different salt`() {
        val a = BackupCrypto.deriveKey("pw".toCharArray(), ByteArray(BackupFormat.SALT_LEN) { 1 }, fastIters).encoded
        val b = BackupCrypto.deriveKey("pw".toCharArray(), ByteArray(BackupFormat.SALT_LEN) { 2 }, fastIters).encoded
        assertFalse(a.contentEquals(b))
    }

    @Test
    fun `open rejects a non-backup blob before doing any crypto`() {
        assertThrows(BackupException.BadMagic::class.java) {
            BackupCrypto.open("pw".toCharArray(), ByteArray(200) { 0x55 })
        }
    }

    @Test
    fun `large sqlite snapshot round-trips`() {
        val payload = BackupPayload(
            identity = ByteArray(BackupFormat.IDENTITY_LEN) { 5 },
            sqlite = ByteArray(2 * 1024 * 1024) { (it % 256).toByte() }, // 2 MB
            srcVersionCode = 1,
            createdAtMs = 123L,
        )
        val file = BackupCrypto.seal("pw".toCharArray(), payload, iterations = fastIters)
        assertTrue(file.size > 2 * 1024 * 1024)
        assertEquals(payload, BackupCrypto.open("pw".toCharArray(), file))
    }
}
