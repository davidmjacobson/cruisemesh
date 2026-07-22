package com.cruisemesh.app.identity.backup

import uniffi.cruisemesh_core.BackupPassphraseStrength
import uniffi.cruisemesh_core.backupMinPassphraseLength
import uniffi.cruisemesh_core.backupPassphraseStrength

/**
 * Passphrase policy for account backups. The
 * backup file *is* the account, so a weak passphrase is a stolen identity —
 * this gates the export button and drives a strength meter. Pure logic, no
 * Android types, so it unit-tests directly.
 */
object BackupPassphrase {

    /** Minimum length we accept at all. A passphrase, not a PIN. */
    val MIN_LENGTH: Int
        get() = backupMinPassphraseLength().toInt()

    enum class Strength { TOO_SHORT, WEAK, FAIR, STRONG }

    /** True once the passphrase is long enough to be allowed (see [strength] for quality). */
    fun isAcceptable(passphrase: CharArray): Boolean = passphrase.size >= MIN_LENGTH

    /**
     * A coarse strength estimate from length + character-class variety. Not a
     * substitute for a real estimator, just enough to nudge the user off
     * obviously weak inputs.
     */
    fun strength(passphrase: CharArray): Strength {
        return when (backupPassphraseStrength(passphrase.concatToString())) {
            BackupPassphraseStrength.TOO_SHORT -> Strength.TOO_SHORT
            BackupPassphraseStrength.WEAK -> Strength.WEAK
            BackupPassphraseStrength.FAIR -> Strength.FAIR
            BackupPassphraseStrength.STRONG -> Strength.STRONG
        }
    }
}
