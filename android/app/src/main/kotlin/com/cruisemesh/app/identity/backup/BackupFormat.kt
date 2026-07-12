package com.cruisemesh.app.identity.backup

/**
 * On-disk format for a `.cmbak` account backup (LOCAL_BACKUP_RESTORE.md §4).
 *
 * A backup file is:
 *
 * ```
 * magic        "CMBAK1\0"   7 bytes
 * version      u8           = FORMAT_VERSION
 * kdf_id       u8           which passphrase KDF produced the key (see KdfId)
 * kdf_params   16 bytes     KDF-specific; PBKDF2 = iters u32 BE + 12 reserved
 * salt         16 bytes     CSPRNG, KDF salt
 * nonce        12 bytes     CSPRNG, AES-GCM IV
 * ciphertext   variable     AES-256-GCM(key, nonce, innerPlaintext, aad = header[0..HEADER_LEN))
 * tag          16 bytes     GCM auth tag (appended to ciphertext by the cipher)
 * ```
 *
 * The header bytes are fed to GCM as AAD, so the format/version/KDF params
 * cannot be tampered with without failing decryption. Kept free of
 * Android/Compose types so it is unit-testable on the JVM, same pattern as
 * `OverlayPlacement` / `MeshRouterState`.
 */
object BackupFormat {
    val MAGIC = byteArrayOf('C'.code.toByte(), 'M'.code.toByte(), 'B'.code.toByte(), 'A'.code.toByte(), 'K'.code.toByte(), '1'.code.toByte(), 0)

    const val FORMAT_VERSION = 1

    /** Byte length of the plaintext header that precedes the ciphertext (and doubles as GCM AAD). */
    const val HEADER_LEN = 7 + 1 + 1 + 16 + 16 + 12 // magic + version + kdf_id + kdf_params + salt + nonce = 53

    const val KDF_PARAMS_LEN = 16
    const val SALT_LEN = 16
    const val NONCE_LEN = 12
    const val GCM_TAG_LEN = 16

    /** Ed25519 + X25519 identity blob length: userId + signPk + signSk + agreePk + agreeSk. */
    const val IDENTITY_LEN = 16 + 32 + 32 + 32 + 32 // 144

    const val INNER_VERSION = 1

    /**
     * Passphrase key-derivation function ids. v1 ships [PBKDF2_HMAC_SHA256]
     * (zero extra dependencies, in the platform crypto provider); the stronger
     * ids are reserved so a later app can read old files while writing new ones
     * with a memory-hard KDF.
     */
    object KdfId {
        const val ARGON2ID = 1 // reserved (future)
        const val SCRYPT = 2 // reserved (future)
        const val PBKDF2_HMAC_SHA256 = 3 // written by v1
    }

    /** Default PBKDF2 work factor written by v1. Tune upward as devices get faster. */
    const val PBKDF2_DEFAULT_ITERATIONS = 600_000

    const val DERIVED_KEY_BITS = 256
}

/** Everything a restore needs: the raw identity, the message-store snapshot, and provenance metadata. */
data class BackupPayload(
    val identity: ByteArray,
    val sqlite: ByteArray,
    val srcVersionCode: Int,
    val createdAtMs: Long,
) {
    // Value semantics on the byte arrays so tests (and equals-based logic) behave.
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is BackupPayload) return false
        return identity.contentEquals(other.identity) &&
            sqlite.contentEquals(other.sqlite) &&
            srcVersionCode == other.srcVersionCode &&
            createdAtMs == other.createdAtMs
    }

    override fun hashCode(): Int {
        var result = identity.contentHashCode()
        result = 31 * result + sqlite.contentHashCode()
        result = 31 * result + srcVersionCode
        result = 31 * result + createdAtMs.hashCode()
        return result
    }
}

/** Parsed outer header plus the location of the ciphertext within the file. */
data class BackupHeader(
    val kdfId: Int,
    val kdfParams: ByteArray,
    val salt: ByteArray,
    val nonce: ByteArray,
    /** The full header bytes [0, HEADER_LEN), used as GCM AAD. */
    val aad: ByteArray,
    /** ciphertext+tag bytes following the header. */
    val ciphertext: ByteArray,
) {
    /** PBKDF2 iteration count read from [kdfParams] (first 4 bytes, big-endian). */
    val pbkdf2Iterations: Int
        get() = ((kdfParams[0].toInt() and 0xFF) shl 24) or
            ((kdfParams[1].toInt() and 0xFF) shl 16) or
            ((kdfParams[2].toInt() and 0xFF) shl 8) or
            (kdfParams[3].toInt() and 0xFF)
}

/** Typed failures surfaced to the UI so backup/restore never leaks a raw exception or crashes. */
sealed class BackupException(message: String) : Exception(message) {
    object BadMagic : BackupException("Not a CruiseMesh backup file")
    data class UnsupportedVersion(val version: Int) : BackupException("Unsupported backup format version $version")
    data class UnsupportedKdf(val kdfId: Int) : BackupException("Unsupported backup key derivation ($kdfId)")
    object Truncated : BackupException("Backup file is truncated or corrupt")
    object WrongPassphraseOrCorrupt : BackupException("Incorrect passphrase or corrupt backup file")

    /** The backup was written by a newer app than the one restoring it; refuse rather than risk a schema downgrade. */
    data class NewerBackup(val srcVersionCode: Int, val appVersionCode: Int) :
        BackupException("This backup is from a newer app version ($srcVersionCode > $appVersionCode); update CruiseMesh first")
}
