package com.cruisemesh.app.identity.backup

import java.io.ByteArrayInputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BackupServiceTest {
    @Test
    fun `bounded reader rejects bytes beyond its limit`() {
        val error = runCatching {
            ByteArrayInputStream(ByteArray(9)).readBackupBytes(8)
        }.exceptionOrNull()

        assertTrue(error is BackupFileTooLargeException)
        val exact = ByteArray(8) { it.toByte() }
        assertArrayEquals(exact, ByteArrayInputStream(exact).readBackupBytes(8))
    }

    @Test
    fun `a release build refuses a backup from a newer build`() {
        assertTrue(refuseNewerBackup(srcVersionCode = 2, appVersionCode = 1, debuggableBuild = false))
        assertFalse(refuseNewerBackup(srcVersionCode = 1, appVersionCode = 1, debuggableBuild = false))
        assertFalse(refuseNewerBackup(srcVersionCode = 1, appVersionCode = 2, debuggableBuild = false))
    }

    @Test
    fun `a debuggable build restores a backup made by a shipped build`() {
        // A local build's version code is a frozen constant older than every
        // release, so the newer-build refusal would block every real backup.
        assertFalse(refuseNewerBackup(srcVersionCode = 2, appVersionCode = 1, debuggableBuild = true))
        assertFalse(refuseNewerBackup(srcVersionCode = 1, appVersionCode = 2, debuggableBuild = true))
    }
}
