package com.cruisemesh.app.identity.backup

import com.cruisemesh.app.R
import uniffi.cruisemesh_core.CoreBackupException

/**
 * What to put on screen when a backup or restore fails.
 *
 * Most failures arrive as a typed core exception whose `message` is an empty
 * string (the generated bindings only build a message from a variant's fields,
 * and the interesting variants carry none). `e.message ?: fallback` therefore
 * never falls back — it renders an empty line, and the user sees the button do
 * nothing at all. The few variants that do have a message spell it in field
 * form ("version=1"), which is not something to show anyone. So every known
 * variant is mapped here to a real sentence instead, and anything unrecognised
 * only uses its own message when that message is actually non-blank.
 */
sealed interface BackupFailureText {
    /** A user-facing string resource. */
    data class Resource(val resId: Int) : BackupFailureText

    /** A message that was already built for the user (already localized). */
    data class Literal(val text: String) : BackupFailureText
}

/**
 * Map a thrown [error] to the sentence the user should read.
 * [fallbackResId] covers unexpected failures that carry no usable message.
 */
fun backupFailureText(error: Throwable, fallbackResId: Int): BackupFailureText = when (error) {
    is CoreBackupException.WrongPassphraseOrCorrupt ->
        BackupFailureText.Resource(R.string.ui_that_passphrase_didn_t_work)
    is CoreBackupException.BadMagic ->
        BackupFailureText.Resource(R.string.ui_that_file_isn_t_a_cruisemesh_backup)
    is CoreBackupException.Truncated ->
        BackupFailureText.Resource(R.string.ui_that_backup_file_is_incomplete)
    is CoreBackupException.UnsupportedVersion,
    is CoreBackupException.UnsupportedKdf,
    is BackupException.NewerBackup ->
        BackupFailureText.Resource(R.string.ui_this_backup_was_made_by_a_newer_version)
    is CoreBackupException.InvalidPayload ->
        BackupFailureText.Resource(R.string.ui_that_backup_couldn_t_be_read)
    else ->
        error.message
            ?.takeIf { it.isNotBlank() }
            ?.let { BackupFailureText.Literal(it) }
            ?: BackupFailureText.Resource(fallbackResId)
}
