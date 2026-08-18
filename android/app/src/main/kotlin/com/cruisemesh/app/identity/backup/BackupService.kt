package com.cruisemesh.app.identity.backup

import android.content.Context
import android.content.pm.ApplicationInfo
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Log
import com.cruisemesh.app.R
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.identity.OnboardingStore
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.friending.FriendsOfFriendsStore
import com.cruisemesh.app.identity.decodeIdentity
import com.cruisemesh.app.identity.encodeIdentity
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.AppStore
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import uniffi.cruisemesh_core.backupMaxFileBytes
import uniffi.cruisemesh_core.BackupContentOptions
import uniffi.cruisemesh_core.BackupInventory
import uniffi.cruisemesh_core.CoreRestorePlan
import uniffi.cruisemesh_core.inspectRestoredMessageStore
import uniffi.cruisemesh_core.sanitizeRestoredMessageStoreWithOptions

/**
 * Android glue for account backup/restore:
 * gathers the identity + message-store snapshot, seals them with
 * [BackupCrypto], and reads/writes the resulting `.cmbak` through the Storage
 * Access Framework. All calls do KDF + crypto + file I/O and MUST run off the
 * main thread (the callers use a background dispatcher).
 *
 * The message-store snapshot comes from the core's transactionally consistent
 * `MessageStore.backup_to` primitive. The core requires a new absolute path so
 * a snapshot request cannot overwrite another local file.
 */
object BackupService {

    private const val STORE_FILENAME = "cruisemesh.sqlite"
    private const val TAG = "BackupService"

    fun inventory(context: Context, nowMs: Long = System.currentTimeMillis()): BackupInventory =
        AppStore.get(context).backupInventory(nowMs)

    /** Build the encrypted `.cmbak` bytes from the current on-device identity and message store. */
    fun buildBackup(
        context: Context,
        passphrase: CharArray,
        options: BackupContentOptions = defaultContentOptions(),
    ): ByteArray {
        val identity = IdentityStore.load(context)
            ?: throw IllegalStateException("No identity on this device to back up")
        val snapshotFile = java.io.File.createTempFile("cruisemesh-backup-", ".sqlite", context.cacheDir)
        snapshotFile.delete()
        val sqliteBytes = try {
            val report = AppStore.get(context).backupToWithOptions(
                snapshotFile.absolutePath,
                options,
                System.currentTimeMillis(),
            )
            Log.i(
                TAG,
                "Prepared backup snapshot; removed messages=${report.removedMessageCount}, " +
                    "ownPending=${report.removedPendingOwnDeliveryCount}, " +
                    "courier=${report.removedCourierDeliveryCount}, " +
                    "expired=${report.removedExpiredDeliveryCount}, " +
                    "connectionEvents=${report.removedConnectionEventCount}",
            )
            snapshotFile.inputStream().use { input -> readBackupBytes(context, input) }
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
            friendsOfFriendsEnabled = FriendsOfFriendsStore.isEnabled(context),
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
    fun previewBackup(
        context: Context,
        fileBytes: ByteArray,
        passphrase: CharArray,
    ): BackupPreview {
        val payload = openAndValidatePayload(context, fileBytes, passphrase)
        val inventory = withStagedSqlite(context, payload.sqlite) { staged ->
            inspectRestoredMessageStore(staged.absolutePath, System.currentTimeMillis())
        }
        return BackupPreview(
            inventory = inventory,
            createdAtMs = payload.createdAtMs,
            sourceVersionCode = payload.srcVersionCode,
            // §9's closing paragraph: opening a `.cmbak` on a fresh install is
            // two different intents wearing one word, and the person has to be
            // the one who picks. Read on the decrypt this screen already
            // performs, so choosing does not cost a second passphrase entry.
            plans = BackupCrypto.restorePlans(payload),
        )
    }

    fun restoreBackup(
        context: Context,
        fileBytes: ByteArray,
        passphrase: CharArray,
        options: BackupContentOptions = defaultContentOptions(),
    ) {
        val payload = openAndValidatePayload(context, fileBytes, passphrase)

        val identity = decodeIdentity(payload.identity)
        // Install the message store from a staged file. Do NOT round-trip the
        // sanitized DB through a second heap ByteArray: on a default 256 MiB
        // heap the encrypted backup + decrypted sqlite already fill most of
        // the process, and `File.readBytes()` of an ~88 MiB store OOMs
        // (Pixel 10, 2026-08-07).
        installSanitizedMessageStore(context, payload.sqlite, options)

        // Every preference write below must be durable. The caller restarts by
        // hard-exiting the process, which outruns an `apply()` and used to drop
        // the restored identity, display name and relay endpoint on the floor,
        // leaving a phone with its old messages but a brand-new `user_id`.
        //
        // Re-wrap the restored keys under THIS device's fresh Keystore key.
        IdentityStore.save(context, identity, durable = true)
        payload.displayName?.let { ProfileStore.saveDisplayName(context, it, durable = true) }
        ProfilePhotoStore.restoreBackupBytes(context, payload.ownAvatar)
        ProfileStore.restoreOwnAvatarEpoch(context, payload.ownAvatarEpoch)
        if (payload.relayUrl != null && payload.relayToken != null) {
            RelayConfigStore.save(context, payload.relayUrl, payload.relayToken, durable = true)
        }
        RelayConfigStore.setShareOnline(context, payload.shareOnline, durable = true)
        FriendsOfFriendsStore.restoreEnabled(context, payload.friendsOfFriendsEnabled)
        OnboardingStore.markCompleted(context, durable = true)
    }

    fun defaultContentOptions() = BackupContentOptions(
        includeMessageHistory = true,
        includePendingDeliveriesForOthers = false,
    )

    /** Read a SAF document without ever accumulating more than the core's backup cap. */
    fun readBytes(context: Context, uri: Uri): ByteArray {
        val maxBytes = backupReadLimit()
        val declaredLength = context.contentResolver
            .openAssetFileDescriptor(uri, "r")
            ?.use { it.declaredLength }
        if (declaredLength != null && declaredLength > maxBytes) {
            throw IOException(context.getString(R.string.ui_this_backup_file_is_too_large))
        }
        return context.contentResolver.openInputStream(uri)?.use { input ->
            readBackupBytes(context, input)
        } ?: throw IllegalStateException("Could not open backup file")
    }

    /**
     * The name to show for a picked document. A SAF uri's last path segment is
     * a provider document id, which for a file the app itself wrote is often
     * just a number ("152"); the provider's display-name column is the actual
     * filename. Falls back to the path segment when no provider answers.
     */
    fun displayName(context: Context, uri: Uri): String? {
        val fromProvider = runCatching {
            context.contentResolver
                .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
                ?.use { cursor ->
                    val column = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (column >= 0 && cursor.moveToFirst()) cursor.getString(column) else null
                }
        }.getOrNull()
        return fromProvider?.takeIf { it.isNotBlank() }
            ?: uri.lastPathSegment?.substringAfterLast('/')
    }

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
        // minSdk is 31, so longVersionCode (added in API 28) is always available.
        return info.longVersionCode.toInt()
    }

    /**
     * True for a locally built app. Read from the installed package rather than
     * `BuildConfig`, which this module does not generate.
     */
    private fun isDebuggableBuild(context: Context): Boolean =
        (context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0

    private fun readBackupBytes(context: Context, input: InputStream): ByteArray = try {
        input.readBackupBytes(backupReadLimit())
    } catch (_: BackupFileTooLargeException) {
        throw IOException(context.getString(R.string.ui_this_backup_file_is_too_large))
    }

    private fun backupReadLimit(): Int {
        val limit = backupMaxFileBytes()
        check(limit <= Int.MAX_VALUE.toULong())
        return limit.toInt()
    }

    /**
     * Validate and migrate the SQLite payload away from the installed store,
     * then remove courier and relay-runtime rows before the restored identity
     * can start any transports. This also protects legacy full-DB backups that
     * predate the same scrub in `MessageStore.backupTo`.
     */
    private fun openAndValidatePayload(
        context: Context,
        fileBytes: ByteArray,
        passphrase: CharArray,
    ): BackupPayload {
        val payload = BackupCrypto.open(passphrase, fileBytes)
        val appVersion = appVersionCode(context)
        if (refuseNewerBackup(payload.srcVersionCode, appVersion, isDebuggableBuild(context))) {
            throw BackupException.NewerBackup(payload.srcVersionCode, appVersion)
        }
        if (payload.srcVersionCode > appVersion) {
            Log.w(
                TAG,
                "Reading a backup made by version ${payload.srcVersionCode} on $appVersion; " +
                    "this debuggable build uses a frozen version code.",
            )
        }
        return payload
    }

    /**
     * Sanitize the restored SQLite on a temp path, then place it at the live
     * store path without ever re-materializing the whole DB as a ByteArray.
     */
    private fun installSanitizedMessageStore(
        context: Context,
        sqlite: ByteArray,
        options: BackupContentOptions,
    ) {
        val sqliteFile = context.filesDir.resolve(STORE_FILENAME)
        clearMessageStoreSiblings(context)

        if (sqlite.isEmpty()) {
            sqliteFile.takeIf { it.exists() }?.delete()
            return
        }

        val staged = java.io.File.createTempFile("cruisemesh-restore-", ".sqlite", context.cacheDir)
        var movedToDestination = false
        try {
            staged.writeBytes(sqlite)
            val report = sanitizeRestoredMessageStoreWithOptions(
                staged.absolutePath,
                options,
                System.currentTimeMillis(),
            )
            Log.i(
                TAG,
                "Sanitized restored store; removed messages=${report.removedMessageCount}, " +
                    "ownPending=${report.removedPendingOwnDeliveryCount}, " +
                    "courier=${report.removedCourierDeliveryCount}, " +
                    "expired=${report.removedExpiredDeliveryCount}, " +
                    "connectionEvents=${report.removedConnectionEventCount}",
            )

            // Prefer a same-filesystem rename (zero extra heap). Fall back to a
            // streamed copy if rename is refused (cross-device, or dest open).
            sqliteFile.takeIf { it.exists() }?.delete()
            if (staged.renameTo(sqliteFile)) {
                movedToDestination = true
            } else {
                staged.inputStream().use { input ->
                    sqliteFile.outputStream().use { output -> input.copyTo(output) }
                }
            }
        } finally {
            for (suffix in listOf("-journal", "-wal", "-shm")) {
                java.io.File(staged.path + suffix).takeIf { it.exists() }?.delete()
            }
            if (!movedToDestination) {
                staged.takeIf { it.exists() }?.delete()
            }
        }
    }

    private fun clearMessageStoreSiblings(context: Context) {
        for (suffix in listOf("-journal", "-wal", "-shm")) {
            context.filesDir.resolve(STORE_FILENAME + suffix).takeIf { it.exists() }?.delete()
        }
    }

    private inline fun <T> withStagedSqlite(
        context: Context,
        sqlite: ByteArray,
        block: (java.io.File) -> T,
    ): T {
        val staged = java.io.File.createTempFile("cruisemesh-restore-", ".sqlite", context.cacheDir)
        return try {
            staged.writeBytes(sqlite)
            block(staged)
        } finally {
            for (suffix in listOf("", "-journal", "-wal", "-shm")) {
                staged.resolveSibling(staged.name + suffix).takeIf { it.exists() }?.delete()
            }
        }
    }
}

data class BackupPreview(
    val inventory: BackupInventory,
    val createdAtMs: Long,
    val sourceVersionCode: Int,
    /**
     * §9's "Replace this device" and "Link as new device", in core's order.
     * Empty is never expected — `core_backup_restore_plans` always returns both
     * — but a shell that finds it empty falls back to the old single-meaning
     * restore rather than showing a fork with no branches.
     */
    val plans: List<CoreRestorePlan> = emptyList(),
)

internal class BackupFileTooLargeException : IOException()

internal fun InputStream.readBackupBytes(maxBytes: Int): ByteArray {
    require(maxBytes >= 0)
    val output = ByteArrayOutputStream(minOf(maxBytes, 64 * 1024))
    val buffer = ByteArray(64 * 1024)
    var total = 0
    while (true) {
        val read = read(buffer)
        if (read < 0) break
        if (read == 0) continue
        if (read > maxBytes - total) throw BackupFileTooLargeException()
        output.write(buffer, 0, read)
        total += read
    }
    return output.toByteArray()
}
