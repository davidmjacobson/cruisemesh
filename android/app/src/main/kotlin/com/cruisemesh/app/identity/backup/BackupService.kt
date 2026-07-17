package com.cruisemesh.app.identity.backup

import android.content.Context
import android.net.Uri
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.identity.OnboardingStore
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.identity.decodeIdentity
import com.cruisemesh.app.identity.encodeIdentity
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.AppStore

/**
 * Android glue for account backup/restore (LOCAL_BACKUP_RESTORE.md §6/§7):
 * gathers the identity + message-store snapshot, seals them with
 * [BackupCrypto], and reads/writes the resulting `.cmbak` through the Storage
 * Access Framework. All calls do KDF + crypto + file I/O and MUST run off the
 * main thread (the callers use a background dispatcher).
 *
 * The message-store snapshot is a plain copy of `cruisemesh.sqlite`. The core
 * opens it in SQLite's default rollback-journal mode, so between (Mutex-guarded,
 * infrequent) write transactions the single file is self-contained and a hot
 * copy is consistent. The durable, always-atomic version is a core
 * `MessageStore.backup_to` (VACUUM INTO); see the spec's follow-up checklist.
 */
object BackupService {

    private const val STORE_FILENAME = "cruisemesh.sqlite"

    /** Build the encrypted `.cmbak` bytes from the current on-device identity and message store. */
    fun buildBackup(context: Context, passphrase: CharArray): ByteArray {
        val identity = IdentityStore.load(context)
            ?: throw IllegalStateException("No identity on this device to back up")
        val snapshotFile = java.io.File.createTempFile("cruisemesh-backup-", ".sqlite", context.cacheDir)
        snapshotFile.delete()
        val sqliteBytes = try {
            AppStore.get(context).backupTo(snapshotFile.absolutePath)
            snapshotFile.readBytes()
        } finally {
            snapshotFile.delete()
        }
        val relay = RelayConfigStore.load(context)

        val payload = BackupPayload(
            identity = encodeIdentity(identity),
            sqlite = sqliteBytes,
            srcVersionCode = appVersionCode(context),
            createdAtMs = System.currentTimeMillis(),
            displayName = ProfileStore.loadDisplayName(context),
            ownAvatar = ProfilePhotoStore.loadBackupBytes(context),
            ownAvatarEpoch = ProfileStore.loadOwnAvatarEpoch(context),
            relayUrl = relay?.relayUrl,
            relayToken = relay?.relayToken,
            shareOnline = RelayConfigStore.shareOnline(context),
        )
        return BackupCrypto.seal(passphrase, payload)
    }

    /**
     * Decrypt and install a backup. Intended for onboarding (fresh install),
     * where the message store has not been opened yet, so overwriting the DB
     * file and re-seeding the identity is safe. Throws a typed
     * [BackupException] on a bad file / wrong passphrase / newer-version backup;
     * on success the caller should restart the app so the identity and store are
     * re-read cleanly.
     */
    fun restoreBackup(context: Context, fileBytes: ByteArray, passphrase: CharArray) {
        val payload = BackupCrypto.open(passphrase, fileBytes)

        val appVersion = appVersionCode(context)
        if (payload.srcVersionCode > appVersion) {
            throw BackupException.NewerBackup(payload.srcVersionCode, appVersion)
        }

        val identity = decodeIdentity(payload.identity)

        // Replace the message store file. Clear any stale journal siblings so a
        // half-written journal from a prior install can't be replayed over the
        // restored DB.
        val sqliteFile = context.filesDir.resolve(STORE_FILENAME)
        for (suffix in listOf("-journal", "-wal", "-shm")) {
            context.filesDir.resolve(STORE_FILENAME + suffix).takeIf { it.exists() }?.delete()
        }
        if (payload.sqlite.isNotEmpty()) {
            sqliteFile.writeBytes(payload.sqlite)
        } else {
            sqliteFile.takeIf { it.exists() }?.delete()
        }

        // Re-wrap the restored keys under THIS device's fresh Keystore key.
        IdentityStore.save(context, identity)
        payload.displayName?.let { ProfileStore.saveDisplayName(context, it) }
        ProfilePhotoStore.restoreBackupBytes(context, payload.ownAvatar)
        ProfileStore.restoreOwnAvatarEpoch(context, payload.ownAvatarEpoch)
        if (payload.relayUrl != null && payload.relayToken != null) {
            RelayConfigStore.save(context, payload.relayUrl, payload.relayToken)
        }
        RelayConfigStore.setShareOnline(context, payload.shareOnline)
        OnboardingStore.markCompleted(context)
    }

    /** Read all bytes of a SAF document (the file the user picked to restore from). */
    fun readBytes(context: Context, uri: Uri): ByteArray =
        context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
            ?: throw IllegalStateException("Could not open backup file")

    /** Write bytes to a SAF document (the destination the user chose to save the backup). */
    fun writeBytes(context: Context, uri: Uri, bytes: ByteArray) {
        // "wt" = write + truncate, so re-saving over an existing file replaces it cleanly.
        context.contentResolver.openOutputStream(uri, "wt")?.use { it.write(bytes) }
            ?: throw IllegalStateException("Could not write backup file")
    }

    /** Suggested filename for a new backup, e.g. `cruisemesh-backup-20260712-1530.cmbak`. */
    fun suggestedFileName(nowMs: Long = System.currentTimeMillis()): String {
        val stamp = java.text.SimpleDateFormat("yyyyMMdd-HHmm", java.util.Locale.US)
            .format(java.util.Date(nowMs))
        return "cruisemesh-backup-$stamp.cmbak"
    }

    private fun appVersionCode(context: Context): Int {
        val info = context.packageManager.getPackageInfo(context.packageName, 0)
        @Suppress("DEPRECATION")
        return if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.P) {
            info.longVersionCode.toInt()
        } else {
            info.versionCode
        }
    }
}
