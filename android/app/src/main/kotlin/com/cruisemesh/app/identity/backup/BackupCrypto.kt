package com.cruisemesh.app.identity.backup

import uniffi.cruisemesh_core.CoreBackupPayload
import uniffi.cruisemesh_core.coreBackupRestorePlans
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

    /**
     * §9's two meanings of "restore", as core states them.
     *
     * The list is core's, order included — "Link as new device" is deliberately
     * first there, and the surface must not reorder a choice whose ordering is
     * itself the recommendation. Every field of a
     * [uniffi.cruisemesh_core.CoreRestorePlan] is a decision this shell would
     * otherwise have to re-derive, and getting one of them wrong is how a clone
     * happens.
     */
    fun restorePlans(payload: BackupPayload) = coreBackupRestorePlans(payload.toCore())
}

private fun BackupPayload.toCore() = CoreBackupPayload(
    identity, sqlite, srcVersionCode, createdAtMs, displayName, ownAvatar,
    ownAvatarEpoch, relayUrl, relayToken, shareOnline, friendsOfFriendsEnabled,
)

private fun CoreBackupPayload.toPlatform() = BackupPayload(
    identity, sqlite, srcVersionCode, createdAtMs, displayName, ownAvatar,
    ownAvatarEpoch, relayUrl, relayToken, shareOnline, friendsOfFriendsEnabled,
)
