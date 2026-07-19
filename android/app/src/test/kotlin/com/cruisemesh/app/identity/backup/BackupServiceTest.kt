package com.cruisemesh.app.identity.backup

import java.io.ByteArrayInputStream
import org.junit.Assert.assertArrayEquals
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
}
