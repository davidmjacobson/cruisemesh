package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreFailoverResumeArm
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
 *
 * `nowMs` must be `SystemClock.elapsedRealtime()`, the clock `postDelayed`
 * itself counts down on -- measuring the window on the wall clock while the
 * timer runs on a monotonic one lets a clock correction split one burst into
 * two resumes.
 */
internal class FailoverResumeDebounce(windowMs: Long = coreFailoverResumeWindowMs()) {
    private val core = CoreFailoverResumeDebounce.withWindowMs(windowMs)

    val windowMs: Long get() = core.windowMs()

    /**
     * Returns the delay to schedule the resume for plus the token to hand back
     * to [fired], or null when a window that is already armed for [key] will
     * cover this failover too.
     */
    fun request(key: String, nowMs: Long): CoreFailoverResumeArm? = core.request(key, nowMs)

    /**
     * The resume scheduled for [key] as [token] is running; that window is
     * over. A token from a window that has since been replaced is ignored, so
     * a timer landing just as a new window is armed cannot clear the new one.
     */
    fun fired(key: String, token: Long) = core.fired(key, token)

    fun cancel(key: String) = core.cancel(key)

    fun isPending(key: String): Boolean = core.isPending(key)

    fun clear() = core.clear()
}
