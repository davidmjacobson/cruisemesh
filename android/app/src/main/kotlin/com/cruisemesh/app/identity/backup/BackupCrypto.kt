package com.cruisemesh.app.identity.backup

import java.security.SecureRandom
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.PBEKeySpec
import javax.crypto.spec.SecretKeySpec

/**
 * Passphrase-based seal/open for account backups (LOCAL_BACKUP_RESTORE.md §4):
 * a slow KDF turns the passphrase into a 256-bit key, then AES-256-GCM
 * encrypts the [BackupCodec]-framed plaintext with the header as AAD.
 *
 * v1 uses PBKDF2-HMAC-SHA256 — no extra dependency, ships in the platform
 * provider — at a high iteration count. The format carries `kdf_id`, so a
 * later release can write Argon2id/scrypt while still reading these files.
 * Deliberately does NOT use the Android Keystore: the whole point is a key that
 * survives leaving the device (reinstall / new phone), which a hardware-bound
 * Keystore key by design does not. Pure JVM crypto so it unit-tests without a
 * device.
 */
object BackupCrypto {

    private const val KEY_ALGORITHM = "AES"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val PBKDF2_ALGORITHM = "PBKDF2WithHmacSHA256"
    private const val GCM_TAG_BITS = BackupFormat.GCM_TAG_LEN * 8

    private val random = SecureRandom()

    /**
     * Encrypt [payload] under [passphrase], producing a complete `.cmbak` byte
     * array (header + ciphertext + tag). A fresh random salt and nonce are
     * generated per call.
     */
    fun seal(
        passphrase: CharArray,
        payload: BackupPayload,
        iterations: Int = BackupFormat.PBKDF2_DEFAULT_ITERATIONS,
    ): ByteArray {
        val salt = ByteArray(BackupFormat.SALT_LEN).also { random.nextBytes(it) }
        val nonce = ByteArray(BackupFormat.NONCE_LEN).also { random.nextBytes(it) }
        val header = BackupCodec.encodeHeader(
            kdfId = BackupFormat.KdfId.PBKDF2_HMAC_SHA256,
            kdfParams = BackupCodec.pbkdf2Params(iterations),
            salt = salt,
            nonce = nonce,
        )
        val key = deriveKey(passphrase, salt, iterations)
        val cipher = Cipher.getInstance(TRANSFORMATION).apply {
            init(Cipher.ENCRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, nonce))
            updateAAD(header)
        }
        val ciphertext = cipher.doFinal(BackupCodec.encodeInner(payload))
        return header + ciphertext
    }

    /**
     * Decrypt a `.cmbak` [file] under [passphrase]. A wrong passphrase (or any
     * tampering) fails the GCM tag and surfaces as
     * [BackupException.WrongPassphraseOrCorrupt]; structural problems surface
     * as other typed [BackupException]s. Never throws a raw crypto exception.
     */
    fun open(passphrase: CharArray, file: ByteArray): BackupPayload {
        val header = BackupCodec.decodeHeader(file)
        if (header.kdfId != BackupFormat.KdfId.PBKDF2_HMAC_SHA256) {
            throw BackupException.UnsupportedKdf(header.kdfId)
        }
        val key = deriveKey(passphrase, header.salt, header.pbkdf2Iterations)
        val cipher = Cipher.getInstance(TRANSFORMATION).apply {
            init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, header.nonce))
            updateAAD(header.aad)
        }
        val plaintext = try {
            cipher.doFinal(header.ciphertext)
        } catch (_: AEADBadTagException) {
            throw BackupException.WrongPassphraseOrCorrupt
        }
        return BackupCodec.decodeInner(plaintext)
    }

    /** PBKDF2-HMAC-SHA256 → 256-bit AES key. */
    fun deriveKey(passphrase: CharArray, salt: ByteArray, iterations: Int): SecretKeySpec {
        val spec = PBEKeySpec(passphrase, salt, iterations, BackupFormat.DERIVED_KEY_BITS)
        try {
            val keyBytes = SecretKeyFactory.getInstance(PBKDF2_ALGORITHM).generateSecret(spec).encoded
            return SecretKeySpec(keyBytes, KEY_ALGORITHM)
        } finally {
            spec.clearPassword()
        }
    }
}
