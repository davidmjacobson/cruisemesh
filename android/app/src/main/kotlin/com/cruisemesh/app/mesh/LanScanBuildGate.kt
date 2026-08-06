package com.cruisemesh.app.mesh

/**
 * Reserves the gap between accepting a subnet scan and publishing its
 * [RunningSweep]. Candidate materialization happens off-thread, so without
 * this gate a second caller can otherwise enqueue another entire /16 while
 * `runningSweep` is still null.
 */
internal class LanScanBuildGate {
    class Token internal constructor(
        internal val session: Long,
        internal val request: Long,
        internal val generation: Int,
    )

    private var session = 0L
    private var nextRequest = 0L
    private var pendingRequest: Long? = null

    @Synchronized
    fun tryReserve(
        sweepRunning: () -> Boolean,
        nextGeneration: () -> Int,
    ): Token? {
        if (sweepRunning() || pendingRequest != null) return null
        nextRequest = if (nextRequest == Long.MAX_VALUE) 0L else nextRequest + 1L
        pendingRequest = nextRequest
        return Token(session, nextRequest, nextGeneration())
    }

    /**
     * Atomically validate [token] against [reset] and publish the sweep through
     * [activate]. Returning false from [activate] consumes the stale request.
     */
    @Synchronized
    fun activate(token: Token, activate: () -> Boolean): Boolean {
        if (!matches(token)) return false
        val activated = activate()
        pendingRequest = null
        return activated
    }

    @Synchronized
    fun release(token: Token) {
        if (matches(token)) pendingRequest = null
    }

    /** Clear a completed sweep without erasing a replacement activated first. */
    @Synchronized
    fun finishSweep(
        isCurrent: () -> Boolean,
        clear: () -> Unit,
    ): Boolean {
        if (!isCurrent()) return false
        clear()
        return true
    }

    /** Invalidate pending builders and reset the published sweep atomically. */
    @Synchronized
    fun reset(reset: () -> Unit) {
        session = if (session == Long.MAX_VALUE) 0L else session + 1L
        pendingRequest = null
        reset()
    }

    private fun matches(token: Token): Boolean =
        token.session == session && pendingRequest == token.request
}
