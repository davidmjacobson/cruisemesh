package com.cruisemesh.app.debug

/**
 * Confirmation for a diagnostics share. The chooser result is not a read:
 * Drive only opens the FileProvider URI after a folder is chosen.
 *
 * Camera and other clients share that provider, so a match is the armed URI
 * plus a third-party read. Compose-window previews from the system resolver
 * are ignored; backing out of the chooser disarms an unused share.
 */
object DiagnosticsShareHandoff {
    private val INTENT_RESOLVER_PACKAGES = setOf(
        "com.android.intentresolver",
        "com.google.android.intentresolver",
        "com.android.systemui",
    )

    @Volatile
    private var pending: String? = null

    @Volatile
    private var consumed: Boolean = false

    @Volatile
    private var targetChosen: Boolean = false

    @Volatile
    private var listener: (() -> Unit)? = null

    fun expect(key: String) {
        pending = key
        consumed = false
        targetChosen = false
    }

    /** The user picked a share target. Opens before this are resolver preview. */
    fun markTargetChosen() {
        if (pending != null) targetChosen = true
    }

    /** Chooser dismissed with nothing handed off. Leaves an already-read share. */
    fun cancelPending() {
        if (consumed) return
        pending = null
        targetChosen = false
    }

    fun onOpened(
        key: String,
        mode: String = "r",
        callerPackage: String? = null,
        ownPackage: String? = null,
    ) {
        if (!targetChosen) return
        if (!isCountableOpen(mode, callerPackage, ownPackage)) return
        if (key != pending) return
        consumed = true
        listener?.invoke()
    }

    /**
     * True once, if the armed archive was opened. Cleared so a later preview
     * of the same file does not toast again.
     */
    fun takeIfConsumed(): Boolean {
        if (!consumed) return false
        consumed = false
        pending = null
        targetChosen = false
        return true
    }

    fun setListener(newListener: (() -> Unit)?) {
        listener = newListener
    }

    fun reset() {
        pending = null
        consumed = false
        targetChosen = false
        listener = null
    }

    internal fun isCountableOpen(
        mode: String,
        callerPackage: String?,
        ownPackage: String?,
    ): Boolean {
        if (!mode.contains('r')) return false
        if (callerPackage.isNullOrEmpty()) return true
        if (ownPackage != null && callerPackage == ownPackage) return false
        if (callerPackage in INTENT_RESOLVER_PACKAGES) return false
        return true
    }
}
