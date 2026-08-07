package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreFailoverResumeDebounce
import uniffi.cruisemesh_core.coreFailoverResumeWindowMs

/**
 * Thin shell wrapper over the core's per-peer failover-resume debounce (see
 * `CoreFailoverResumeDebounce` for the field bug, the window's sizing and the
 * coalescing rule). Both platforms wrap the same core object so the window
 * cannot drift between them, exactly like [LanHealthTracker] and
 * [ReconnectBackoffTracker].
 *
 * Keys are the peer's UserID hex (`UserIdHex.encode`), never a link address:
 * the whole point is to coalesce the several *links* one logical peer loses in
 * a single radio event down to one resume.
 */
internal class FailoverResumeDebounce(windowMs: Long = coreFailoverResumeWindowMs()) {
    private val core = CoreFailoverResumeDebounce.withWindowMs(windowMs)

    val windowMs: Long get() = core.windowMs()

    /**
     * Returns the delay to schedule the resume for, or null when a window
     * that is already armed for [key] will cover this failover too.
     */
    fun request(key: String, nowMs: Long): Long? = core.request(key, nowMs)

    /** The scheduled resume for [key] is running; the window is over. */
    fun fired(key: String) = core.fired(key)

    fun cancel(key: String) = core.cancel(key)

    fun isPending(key: String): Boolean = core.isPending(key)

    fun clear() = core.clear()
}
