package com.cruisemesh.app.identity.backup

import com.cruisemesh.app.R
import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.CoreBackupException

class BackupFailureTextTest {

    private val fallback = R.string.ui_couldn_t_restore_that_backup

    private fun resIdFor(error: Throwable): Int? =
        (backupFailureText(error, fallback) as? BackupFailureText.Resource)?.resId

    @Test
    fun `core variants with an empty message still get a sentence`() {
        // The trap this guards: these carry message == "", which is non-null,
        // so an `e.message ?: fallback` renders an empty error line.
        assertEquals("", CoreBackupException.WrongPassphraseOrCorrupt().message)
        assertEquals("", CoreBackupException.BadMagic().message)
        assertEquals("", CoreBackupException.Truncated().message)

        assertEquals(
            R.string.ui_that_passphrase_didn_t_work,
            resIdFor(CoreBackupException.WrongPassphraseOrCorrupt()),
        )
        assertEquals(
            R.string.ui_that_file_isn_t_a_cruisemesh_backup,
            resIdFor(CoreBackupException.BadMagic()),
        )
        assertEquals(
            R.string.ui_that_backup_file_is_incomplete,
            resIdFor(CoreBackupException.Truncated()),
        )
    }

    @Test
    fun `field-shaped messages are replaced with plain copy`() {
        assertEquals(
            R.string.ui_this_backup_was_made_by_a_newer_version,
            resIdFor(CoreBackupException.UnsupportedVersion(9u)),
        )
        assertEquals(
            R.string.ui_this_backup_was_made_by_a_newer_version,
            resIdFor(CoreBackupException.UnsupportedKdf(7u)),
        )
        assertEquals(
            R.string.ui_this_backup_was_made_by_a_newer_version,
            resIdFor(BackupException.NewerBackup(2, 1)),
        )
        assertEquals(
            R.string.ui_that_backup_couldn_t_be_read,
            resIdFor(CoreBackupException.InvalidPayload("short")),
        )
    }

    @Test
    fun `unknown errors keep a real message and fall back on a blank one`() {
        assertEquals(
            BackupFailureText.Literal("This backup file is too large."),
            backupFailureText(IOException("This backup file is too large."), fallback),
        )
        assertEquals(BackupFailureText.Resource(fallback), backupFailureText(IOException(""), fallback))
        assertEquals(BackupFailureText.Resource(fallback), backupFailureText(IOException("   "), fallback))
        assertEquals(BackupFailureText.Resource(fallback), backupFailureText(IOException(), fallback))
    }
}
