package com.cruisemesh.app.identity.backup

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BackupPassphraseTest {

    private fun strength(s: String) = BackupPassphrase.strength(s.toCharArray())

    @Test
    fun `short passphrase is unacceptable and rated too short`() {
        assertFalse(BackupPassphrase.isAcceptable("short".toCharArray()))
        assertEquals(BackupPassphrase.Strength.TOO_SHORT, strength("short"))
    }

    @Test
    fun `exactly minimum length is acceptable`() {
        assertTrue(BackupPassphrase.isAcceptable("a".repeat(BackupPassphrase.MIN_LENGTH).toCharArray()))
    }

    @Test
    fun `long single-class passphrase is only weak`() {
        // 12 lowercase letters: long enough to allow, but low variety.
        assertEquals(BackupPassphrase.Strength.WEAK, strength("abcdefghijkl"))
    }

    @Test
    fun `three character classes reach at least fair`() {
        assertEquals(BackupPassphrase.Strength.FAIR, strength("Abcdef12jk"))
    }

    @Test
    fun `long and varied is strong`() {
        assertEquals(BackupPassphrase.Strength.STRONG, strength("Correct-Horse-99-Battery"))
    }
}
