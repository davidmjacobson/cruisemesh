package com.cruisemesh.app.identity.backup

import uniffi.cruisemesh_core.CoreBackupPayload
import uniffi.cruisemesh_core.openBackup
import uniffi.cruisemesh_core.sealBackup

/**
 * Thin Android adapter around the canonical Rust backup implementation.
 * Platform code owns file selection and UI; the portable format and crypto live
 * in `cruisemesh-core` so Android and iOS cannot drift.
 */
object BackupCrypto {

    /**
     * Encrypt [payload] under [passphrase], producing a complete `.cmbak` byte
     * array (header + ciphertext + tag). A fresh random salt and nonce are
     * generated per call.
     */
    fun seal(
        passphrase: CharArray,
        payload: BackupPayload,
        iterations: Int? = null,
    ): ByteArray {
        return sealBackup(passphrase.concatToString(), payload.toCore(), iterations?.toUInt())
    }

    /**
     * Decrypt a `.cmbak` [file] under [passphrase]. A wrong passphrase (or any
     * tampering or malformed input surfaces as a typed Rust core exception.
     */
    fun open(passphrase: CharArray, file: ByteArray): BackupPayload {
        return openBackup(passphrase.concatToString(), file).toPlatform()
    }
}

private fun BackupPayload.toCore() = CoreBackupPayload(
    identity, sqlite, srcVersionCode, createdAtMs, displayName, ownAvatar,
    ownAvatarEpoch, relayUrl, relayToken, shareOnline,
)

private fun CoreBackupPayload.toPlatform() = BackupPayload(
    identity, sqlite, srcVersionCode, createdAtMs, displayName, ownAvatar,
    ownAvatarEpoch, relayUrl, relayToken, shareOnline,
)
