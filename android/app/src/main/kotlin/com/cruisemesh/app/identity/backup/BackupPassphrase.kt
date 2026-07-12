package com.cruisemesh.app.identity.backup

/**
 * Passphrase policy for account backups (LOCAL_BACKUP_RESTORE.md §2.1). The
 * backup file *is* the account, so a weak passphrase is a stolen identity —
 * this gates the export button and drives a strength meter. Pure logic, no
 * Android types, so it unit-tests directly.
 */
object BackupPassphrase {

    /** Minimum length we accept at all. A passphrase, not a PIN. */
    const val MIN_LENGTH = 10

    enum class Strength { TOO_SHORT, WEAK, FAIR, STRONG }

    /** True once the passphrase is long enough to be allowed (see [strength] for quality). */
    fun isAcceptable(passphrase: CharArray): Boolean = passphrase.size >= MIN_LENGTH

    /**
     * A coarse strength estimate from length + character-class variety. Not a
     * substitute for a real estimator, just enough to nudge the user off
     * obviously weak inputs.
     */
    fun strength(passphrase: CharArray): Strength {
        if (passphrase.size < MIN_LENGTH) return Strength.TOO_SHORT

        var lower = false
        var upper = false
        var digit = false
        var symbol = false
        for (c in passphrase) {
            when {
                c.isLowerCase() -> lower = true
                c.isUpperCase() -> upper = true
                c.isDigit() -> digit = true
                else -> symbol = true
            }
        }
        val classes = listOf(lower, upper, digit, symbol).count { it }

        return when {
            passphrase.size >= 16 && classes >= 3 -> Strength.STRONG
            passphrase.size >= 14 || classes >= 3 -> Strength.FAIR
            else -> Strength.WEAK
        }
    }
}
