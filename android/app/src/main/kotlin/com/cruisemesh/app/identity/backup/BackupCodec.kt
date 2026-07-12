package com.cruisemesh.app.identity.backup

import java.nio.BufferUnderflowException
import java.nio.ByteBuffer

/**
 * Pure byte framing for `.cmbak` files (LOCAL_BACKUP_RESTORE.md §4/§5): builds
 * and parses the outer header and the inner (decrypted) plaintext. No crypto
 * and no Android types live here — [BackupCrypto] wraps this with the KDF and
 * AES-GCM. Everything is big-endian (ByteBuffer's default).
 */
object BackupCodec {

    /**
     * Encode the outer header (magic .. nonce). The returned bytes are exactly
     * [BackupFormat.HEADER_LEN] long and are used both as the file prefix and
     * as the GCM AAD.
     */
    fun encodeHeader(kdfId: Int, kdfParams: ByteArray, salt: ByteArray, nonce: ByteArray): ByteArray {
        require(kdfParams.size == BackupFormat.KDF_PARAMS_LEN) { "kdfParams must be ${BackupFormat.KDF_PARAMS_LEN} bytes" }
        require(salt.size == BackupFormat.SALT_LEN) { "salt must be ${BackupFormat.SALT_LEN} bytes" }
        require(nonce.size == BackupFormat.NONCE_LEN) { "nonce must be ${BackupFormat.NONCE_LEN} bytes" }
        return ByteBuffer.allocate(BackupFormat.HEADER_LEN).apply {
            put(BackupFormat.MAGIC)
            put(BackupFormat.FORMAT_VERSION.toByte())
            put(kdfId.toByte())
            put(kdfParams)
            put(salt)
            put(nonce)
        }.array()
    }

    /** PBKDF2 kdf_params: iteration count in the first 4 bytes, rest reserved zero. */
    fun pbkdf2Params(iterations: Int): ByteArray =
        ByteBuffer.allocate(BackupFormat.KDF_PARAMS_LEN).putInt(iterations).array()

    /**
     * Split a full `.cmbak` file into its parsed header + trailing ciphertext,
     * validating magic/version and structural length. Throws a typed
     * [BackupException] on anything malformed.
     */
    fun decodeHeader(file: ByteArray): BackupHeader {
        if (file.size < BackupFormat.HEADER_LEN + BackupFormat.GCM_TAG_LEN) throw BackupException.Truncated
        val magic = file.copyOfRange(0, BackupFormat.MAGIC.size)
        if (!magic.contentEquals(BackupFormat.MAGIC)) throw BackupException.BadMagic

        var offset = BackupFormat.MAGIC.size
        val version = file[offset].toInt() and 0xFF
        offset += 1
        if (version != BackupFormat.FORMAT_VERSION) throw BackupException.UnsupportedVersion(version)

        val kdfId = file[offset].toInt() and 0xFF
        offset += 1
        val kdfParams = file.copyOfRange(offset, offset + BackupFormat.KDF_PARAMS_LEN)
        offset += BackupFormat.KDF_PARAMS_LEN
        val salt = file.copyOfRange(offset, offset + BackupFormat.SALT_LEN)
        offset += BackupFormat.SALT_LEN
        val nonce = file.copyOfRange(offset, offset + BackupFormat.NONCE_LEN)
        offset += BackupFormat.NONCE_LEN

        val aad = file.copyOfRange(0, BackupFormat.HEADER_LEN)
        val ciphertext = file.copyOfRange(BackupFormat.HEADER_LEN, file.size)
        return BackupHeader(kdfId, kdfParams, salt, nonce, aad, ciphertext)
    }

    /** Encode the inner plaintext (metadata front-loaded so it can be read without touching the blobs). */
    fun encodeInner(payload: BackupPayload): ByteArray {
        require(payload.identity.size == BackupFormat.IDENTITY_LEN) {
            "identity must be ${BackupFormat.IDENTITY_LEN} bytes, was ${payload.identity.size}"
        }
        val size = 1 + 4 + 8 + 2 + payload.identity.size + 4 + payload.sqlite.size
        return ByteBuffer.allocate(size).apply {
            put(BackupFormat.INNER_VERSION.toByte())
            putInt(payload.srcVersionCode)
            putLong(payload.createdAtMs)
            putShort(payload.identity.size.toShort())
            put(payload.identity)
            putInt(payload.sqlite.size)
            put(payload.sqlite)
        }.array()
    }

    /** Parse the inner plaintext. Structural problems map to [BackupException.Truncated] (never a raw buffer exception). */
    fun decodeInner(plaintext: ByteArray): BackupPayload {
        try {
            val buf = ByteBuffer.wrap(plaintext)
            val innerVersion = buf.get().toInt() and 0xFF
            if (innerVersion != BackupFormat.INNER_VERSION) throw BackupException.UnsupportedVersion(innerVersion)
            val srcVersionCode = buf.getInt()
            val createdAtMs = buf.getLong()
            val identityLen = buf.getShort().toInt() and 0xFFFF
            if (identityLen != BackupFormat.IDENTITY_LEN) throw BackupException.Truncated
            val identity = ByteArray(identityLen).also { buf.get(it) }
            val sqliteLen = buf.getInt()
            if (sqliteLen < 0 || sqliteLen > buf.remaining()) throw BackupException.Truncated
            val sqlite = ByteArray(sqliteLen).also { buf.get(it) }
            return BackupPayload(identity, sqlite, srcVersionCode, createdAtMs)
        } catch (_: BufferUnderflowException) {
            throw BackupException.Truncated
        } catch (_: IndexOutOfBoundsException) {
            throw BackupException.Truncated
        }
    }
}
