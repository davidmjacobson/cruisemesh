package com.cruisemesh.app.debug

/**
 * Knows when a share target actually took the diagnostics archive.
 *
 * The system share sheet does not tell us that. [Intent.createChooser] returns
 * as soon as Drive (or Files, or mail) is picked, which is before the folder
 * picker and before any bytes move. Drive then reads the FileProvider URI
 * only after the folder is chosen. That read is the first honest "it left
 * this phone" signal, and it is what [CruiseMeshFileProvider] reports here.
 *
 * The screen shows the confirmation on resume so the toast is not buried
 * under Drive's picker.
 */
object DiagnosticsShareHandoff {
    @Volatile
    private var pending: String? = null

    @Volatile
    private var consumed: Boolean = false

    @Volatile
    private var listener: (() -> Unit)? = null

    /** Arm for the URI this share is about to hand to the sheet. */
    fun expect(key: String) {
        pending = key
        consumed = false
    }

    /**
     * The receiving app opened [key]. Ignored unless it is the armed archive:
     * camera captures and other FileProvider clients share this provider.
     */
    fun onOpened(key: String) {
        if (key != pending) return
        consumed = true
        listener?.invoke()
    }

    /**
     * True once, if the armed archive was opened. Clears the pending share so
     * a later open of the same file (preview, retry) does not toast again.
     */
    fun takeIfConsumed(): Boolean {
        if (!consumed) return false
        consumed = false
        pending = null
        return true
    }

    fun setListener(newListener: (() -> Unit)?) {
        listener = newListener
    }

    fun reset() {
        pending = null
        consumed = false
        listener = null
    }
}
