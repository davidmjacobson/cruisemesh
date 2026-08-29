package com.cruisemesh.app.identity.backup

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class BackupFileNameTest {
    @Test
    fun `the provider's display name wins whenever there is one`() {
        assertEquals(
            "cruisemesh-backup-20260712-1530.cmbak",
            BackupFileName.resolve(
                providerDisplayName = "cruisemesh-backup-20260712-1530.cmbak",
                lastPathSegment = "152",
            ),
        )
    }

    @Test
    fun `a blank display name counts as no answer at all`() {
        assertNull(BackupFileName.resolve(providerDisplayName = "   ", lastPathSegment = "152"))
        assertNull(BackupFileName.resolve(providerDisplayName = "", lastPathSegment = null))
    }

    @Test
    fun `a display name is trimmed rather than shown with its padding`() {
        assertEquals(
            "backup.cmbak",
            BackupFileName.resolve(providerDisplayName = "  backup.cmbak\n", lastPathSegment = null),
        )
    }

    @Test
    fun `an opaque document id is never shown`() {
        // The Downloads provider's row number, and the shapes a cloud provider
        // hands back. None of these tell a person which backup they opened.
        for (id in listOf("152", "msf:41", "acc=1;doc=27", "1.2", null)) {
            assertNull(id, BackupFileName.resolve(providerDisplayName = null, lastPathSegment = id))
        }
    }

    @Test
    fun `a document id that is really a path keeps its file name`() {
        assertEquals(
            "cruisemesh-backup-20260712-1530.cmbak",
            BackupFileName.resolve(
                providerDisplayName = null,
                lastPathSegment = "primary:Download/cruisemesh-backup-20260712-1530.cmbak",
            ),
        )
        assertEquals(
            "backup.cmbak",
            BackupFileName.resolve(
                providerDisplayName = null,
                lastPathSegment = "raw:/storage/emulated/0/Download/backup.cmbak",
            ),
        )
    }

    @Test
    fun `a trailing dot or a runaway extension is not a file name`() {
        assertNull(BackupFileName.resolve(providerDisplayName = null, lastPathSegment = "backup."))
        assertNull(BackupFileName.resolve(providerDisplayName = null, lastPathSegment = ".cmbak"))
        assertNull(
            BackupFileName.resolve(
                providerDisplayName = null,
                lastPathSegment = "doc.0123456789abcdef",
            ),
        )
    }
}
