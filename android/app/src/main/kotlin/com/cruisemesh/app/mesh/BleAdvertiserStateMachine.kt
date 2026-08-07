package com.cruisemesh.app.mesh

/** Where [BleAdvertiserStateMachine] thinks the peripheral advertisement is. */
enum class AdvertiserState {
    /** Nothing registered with the framework; nothing is being advertised. */
    IDLE,

    /** `startAdvertising` was called for the current generation; no result yet. */
    STARTING,

    /** The current generation's `onStartSuccess` landed. */
    ADVERTISING,
}

/**
 * What [BlePeripheral] must actually do with `BluetoothLeAdvertiser` for a
 * decision. [stopGeneration] and [startGeneration] are generation numbers,
 * never callbacks: the binder owns the mapping from generation to the one
 * `AdvertiseCallback` instance it registered for that generation, which is the
 * whole point (see the class doc of [BleAdvertiserStateMachine]).
 *
 * When both are set, the stop happens first and the start immediately after.
 *
 * [watchdogInMs], when set, asks the binder to call
 * [BleAdvertiserStateMachine.onWatchdogDue] that many milliseconds from now on
 * a monotonic clock. It is how the machine drives its own recovery -- either
 * "this start has to have reported by then" or "retry the failed start then" --
 * without owning a timer itself. A watchdog that turns out to be unnecessary is
 * harmless: [BleAdvertiserStateMachine.onWatchdogDue] is state-guarded.
 */
data class AdvertiserAction(
    val stopGeneration: Long? = null,
    val startGeneration: Long? = null,
    val watchdogInMs: Long? = null,
) {
    /**
     * Nothing to ask the radio for. Deliberately ignores [watchdogInMs]: a
     * bare timer request is not something the caller "applied" (this is what
     * [BlePeripheral.setAdvertiseDutyMode] logs on).
     */
    val isNone: Boolean get() = stopGeneration == null && startGeneration == null

    companion object {
        val NONE = AdvertiserAction()
    }
}

/**
 * The decision half of [BlePeripheral]'s advertising lifecycle, with no
 * Android imports so it can be unit-tested directly (same pattern as
 * [NotifyFailureTracker] / [MeshRouterState]).
 *
 * ## Why a generation counter exists
 *
 * `BluetoothLeAdvertiser` keys its internal wrapper map on the
 * **`AdvertiseCallback` object** the caller passes. Reusing one singleton
 * callback for every start -- which this code did until 2026-08-07 -- means
 * the app has no way to talk about "this advertising set" versus "the one I
 * just replaced", and three separate silent-permanent-dark sequences fall out
 * of that:
 *
 * 1. **The restart-on-connect path was a framework no-op.** Legacy
 *    connectable advertising stops the instant a central connects (PR #17),
 *    so [BlePeripheral] restarts it from `onConnectionStateChange`. With the
 *    singleton callback still registered, that restart called
 *    `startAdvertising` with an already-registered callback and the framework
 *    answered `ADVERTISE_FAILED_ALREADY_STARTED`, which the old
 *    `onStartFailure` silently mapped back to "advertising = true" with no log
 *    line at all. Field capture: 15 central connects, zero "Advertising
 *    started" lines. Discoverability survived only because the Android stack
 *    re-enabled the legacy set by itself -- the origin of the otherwise
 *    inexplicable `E BluetoothLeAdvertiser: Legacy advertiser should be only
 *    disabled on timeout` lines in the same capture.
 * 2. **The duty-mode restart could stop its own successor.**
 *    [BlePeripheral.setAdvertiseDutyMode] stopped and immediately restarted
 *    advertising. `stopAdvertising(callback)` unregisters generation N, and
 *    the immediate `startAdvertising(callback)` re-registers the *same object*
 *    as generation N+1 -- so generation N's still-live framework wrapper,
 *    whose disable callback lands a moment later, stopped generation N+1. The
 *    old boolean stayed `true`, `beginAdvertising()` early-returned forever,
 *    and the phone was undiscoverable with nothing logged.
 * 3. **A stale `onStartSuccess` could resurrect a dead advertiser.** `stop()`
 *    cleared the boolean, then an in-flight success callback from the
 *    already-stopped generation set it back to `true`, and the next restart
 *    early-returned on a flag describing an advertiser that no longer existed.
 *
 * A monotonically increasing generation, plus a fresh `AdvertiseCallback` per
 * generation on the binder side, makes all three impossible: a stop always
 * names exactly the generation that started, and any callback whose
 * generation is no longer the current one cannot mutate state
 * ([acceptsResultFor]).
 *
 * ## Why the boolean became three states
 *
 * `advertising: Boolean` could not distinguish "a start is in flight" from
 * "not advertising", which is what made the mode-change-during-start bug
 * (below) invisible. [AdvertiserState.STARTING] is the missing state, and
 * [restartPending] is what a start-in-flight does with a request it cannot
 * serve yet.
 *
 * ## Why the machine owns a watchdog
 *
 * Three states plus a generation would still be a one-way trip into the dark
 * if a start could fail (or simply never report) with nothing arranged to try
 * again:
 *
 * - **A failed start.** `ADVERTISE_FAILED_INTERNAL_ERROR` /
 *   `TOO_MANY_ADVERTISERS` leave [AdvertiserState.IDLE]. Every organic
 *   re-trigger is a *link* event -- a teardown, a central connect -- and a
 *   phone that is not advertising gets no link events, so the one thing that
 *   could restart advertising is the thing that being dark prevents. The
 *   boolean this class replaced was accidentally protected here: it never
 *   really stopped anything, so the framework's own legacy re-enable covered
 *   for it.
 * - **A start that never reports.** If the framework registers the set but
 *   never delivers a callback, [AdvertiserState.STARTING] would absorb every
 *   later request forever -- strictly worse than the boolean, which retried on
 *   the next teardown.
 *
 * So [onStartFailed] arms a bounded re-arm ([RETRY_MIN_DELAY_MS], doubling per
 * consecutive failure, capped at [RETRY_MAX_DELAY_MS]) and a start that has not
 * reported within [START_WATCHDOG_MS] is force-retired and retried the same
 * way. Both come back to the binder as [AdvertiserAction.watchdogInMs]. The
 * retry never gives up: a phone that stops advertising is invisible to the
 * whole fleet, so the capped-rate retry is the conservative choice, and a
 * genuine full stop ([onStopRequested]) clears it.
 *
 * All methods are `@Synchronized`: advertise callbacks arrive on binder
 * threads. This is a leaf monitor -- it never calls out -- so it cannot
 * deadlock with [BlePeripheral]'s own locks.
 *
 * Every `nowMs` argument must come from a monotonic clock
 * (`SystemClock.elapsedRealtime()`), the same one the binder's timer counts
 * down on.
 */
class BleAdvertiserStateMachine(initialMode: RadioDutyMode = RadioDutyMode.LOW_POWER) {

    companion object {
        /**
         * How long a `startAdvertising` may sit without `onStartSuccess` or
         * `onStartFailure` before the generation is force-retired and retried.
         * The framework answers in milliseconds when it answers at all, so this
         * is generous on purpose: it is a stuck-state breaker, not a deadline.
         */
        const val START_WATCHDOG_MS = 8_000L

        /** First re-arm delay after a failed (or stuck) start. */
        const val RETRY_MIN_DELAY_MS = 2_000L

        /**
         * Ceiling for the doubling re-arm delay. Bounds how often a phone
         * whose adapter is refusing advertisements retries, without ever
         * abandoning discoverability.
         */
        const val RETRY_MAX_DELAY_MS = 60_000L
    }

    private var state = AdvertiserState.IDLE
    private var generation = 0L
    private var desiredMode = initialMode

    /** When the [AdvertiserState.STARTING] generation was handed to the framework. */
    private var startedAtMs = 0L

    /** When the next re-arm after a failed start is due, or null if none is. */
    private var retryDueAtMs: Long? = null

    /** Consecutive failed starts, for the doubling re-arm delay. */
    private var failureStreak = 0

    /**
     * A restart was asked for while a start was still in flight. It cannot be
     * served now (stopping a generation that has not reported yet would race
     * the framework's own answer), so it is remembered and applied the moment
     * the in-flight start settles -- this is what keeps a duty-mode change
     * from being silently swallowed, the third bug in the class doc.
     */
    private var restartPending = false

    @Synchronized
    fun state(): AdvertiserState = state

    /** The generation a caller should attribute a brand-new callback to. */
    @Synchronized
    fun generation(): Long = generation

    /** The [RadioDutyMode] the next started generation must be built with. */
    @Synchronized
    fun desiredMode(): RadioDutyMode = desiredMode

    /** Visible for tests: a restart is queued behind an in-flight start. */
    @Synchronized
    fun hasRestartPending(): Boolean = restartPending

    /** Visible for tests: a re-arm after a failed start is scheduled. */
    @Synchronized
    fun hasRetryPending(): Boolean = retryDueAtMs != null

    /**
     * Whether a `onStartSuccess`/`onStartFailure` callback tagged
     * [callbackGeneration] is still the one being waited on. False means the
     * callback belongs to a generation that was already stopped, replaced, or
     * answered -- it must be logged and dropped, never allowed to write state.
     */
    @Synchronized
    fun acceptsResultFor(callbackGeneration: Long): Boolean =
        state == AdvertiserState.STARTING && callbackGeneration == generation

    /**
     * "Make sure we are advertising." Idempotent: a start already in flight or
     * already succeeded is left alone, which is what keeps repeated calls
     * (every link teardown calls this) from thrashing the advertiser. A start
     * request while a re-arm is pending starts now and drops the re-arm -- the
     * caller has a fresher reason to advertise than the timer did.
     */
    @Synchronized
    fun onStartRequested(nowMs: Long): AdvertiserAction = when (state) {
        AdvertiserState.IDLE -> beginStart(nowMs, stopGeneration = null)
        AdvertiserState.STARTING, AdvertiserState.ADVERTISING -> AdvertiserAction.NONE
    }

    /**
     * A central connected, so the framework has already stopped the legacy
     * advertising set underneath us (PR #17). The current generation is dead
     * even though nothing told us so: retire it explicitly -- which also
     * unregisters its callback, so the framework's wrapper for it can never
     * outlive it and disable a later generation -- and start a fresh one.
     */
    @Synchronized
    fun onConnectRestartRequested(nowMs: Long): AdvertiserAction = forceRestart(nowMs)

    /**
     * Battery: [RadioPowerPolicy]'s latest decision. A no-op if the mode is
     * unchanged, so callers can call it on every policy tick.
     *
     * The requested mode is recorded *unconditionally*, and separately from
     * whether it can be applied right now:
     *
     * - [AdvertiserState.IDLE]: nothing to restart; the next started
     *   generation is built with the new mode.
     * - [AdvertiserState.STARTING]: the in-flight generation was already built
     *   with the old mode, so it is queued for restart the moment it settles.
     *   The old code wrote the field and then early-returned on
     *   `!advertising`, which recorded a mode change it never applied.
     * - [AdvertiserState.ADVERTISING]: restart now.
     */
    @Synchronized
    fun onDutyModeRequested(mode: RadioDutyMode, nowMs: Long): AdvertiserAction {
        if (mode == desiredMode) return AdvertiserAction.NONE
        desiredMode = mode
        return when (state) {
            // A policy tick is not a request to advertise: if the advertiser
            // is down, only the mode is recorded, exactly as before. Any
            // pending re-arm keeps its own schedule and picks the new mode up.
            AdvertiserState.IDLE -> AdvertiserAction.NONE
            AdvertiserState.STARTING, AdvertiserState.ADVERTISING -> forceRestart(nowMs)
        }
    }

    /**
     * The current generation's `startAdvertising` succeeded. Returns the
     * follow-up restart if one was queued behind it.
     */
    @Synchronized
    fun onStartSucceeded(callbackGeneration: Long, nowMs: Long): AdvertiserAction {
        if (!acceptsResultFor(callbackGeneration)) return AdvertiserAction.NONE
        state = AdvertiserState.ADVERTISING
        failureStreak = 0
        retryDueAtMs = null
        return if (restartPending) forceRestart(nowMs) else AdvertiserAction.NONE
    }

    /**
     * The current generation's `startAdvertising` failed. We are not
     * advertising -- including for `ADVERTISE_FAILED_ALREADY_STARTED`, which a
     * fresh callback per start makes impossible by construction and which the
     * binder therefore logs as unexpected. Mapping that code to "advertising"
     * is precisely how this class's first bug stayed invisible.
     *
     * A re-arm is scheduled (see the class doc): the organic re-triggers are
     * all link events, and a phone that is not advertising stops getting link
     * events, so "the next teardown will retry" is not a recovery plan.
     */
    @Synchronized
    fun onStartFailed(callbackGeneration: Long, nowMs: Long): AdvertiserAction {
        if (!acceptsResultFor(callbackGeneration)) return AdvertiserAction.NONE
        restartPending = false
        return AdvertiserAction(watchdogInMs = armRetry(nowMs))
    }

    /**
     * Full stop (the peripheral role is going away). Retires the current
     * generation whether or not its start ever reported, so a callback still
     * in flight lands as stale and cannot resurrect a dead advertiser, and
     * drops any pending re-arm -- a stopped peripheral must stay stopped.
     */
    @Synchronized
    fun onStopRequested(): AdvertiserAction {
        restartPending = false
        retryDueAtMs = null
        failureStreak = 0
        val stopGeneration = if (state == AdvertiserState.IDLE) null else generation
        // Retire unconditionally: the point is that nothing tagged with the
        // old generation is ever accepted again.
        generation += 1
        state = AdvertiserState.IDLE
        return AdvertiserAction(stopGeneration = stopGeneration)
    }

    /**
     * The binder's timer fired. This is the machine's only self-recovery
     * trigger, and it covers both stuck-state cases:
     *
     * - [AdvertiserState.STARTING] past [START_WATCHDOG_MS]: the framework
     *   never answered. Retire the generation (which unregisters its callback,
     *   so a very late answer lands as stale) and re-arm like any other failed
     *   start.
     * - [AdvertiserState.IDLE] with a re-arm due: start a fresh generation.
     *
     * Anything else -- including a tick that arrives early, or one left over
     * from a state that has since moved on -- either reschedules itself for the
     * remaining time or is a no-op, so spurious ticks are free and the binder
     * never has to reason about cancelling them.
     */
    @Synchronized
    fun onWatchdogDue(nowMs: Long): AdvertiserAction = when (state) {
        AdvertiserState.ADVERTISING -> AdvertiserAction.NONE
        AdvertiserState.STARTING -> {
            val remaining = START_WATCHDOG_MS - elapsedSince(startedAtMs, nowMs)
            if (remaining > 0) {
                AdvertiserAction(watchdogInMs = remaining)
            } else {
                // Retire the unanswered generation *and* re-arm rather than
                // restarting instantly: whatever wedged the framework is
                // unlikely to be fixed microseconds later, and the doubling
                // delay is what bounds the retry rate if it is not.
                val stuck = generation
                restartPending = false
                AdvertiserAction(stopGeneration = stuck, watchdogInMs = armRetry(nowMs))
            }
        }
        AdvertiserState.IDLE -> {
            val due = retryDueAtMs
            when {
                due == null -> AdvertiserAction.NONE
                nowMs - due >= 0 -> beginStart(nowMs, stopGeneration = null)
                else -> AdvertiserAction(watchdogInMs = due - nowMs)
            }
        }
    }

    private fun forceRestart(nowMs: Long): AdvertiserAction = when (state) {
        AdvertiserState.IDLE -> beginStart(nowMs, stopGeneration = null)
        AdvertiserState.STARTING -> {
            restartPending = true
            AdvertiserAction.NONE
        }
        AdvertiserState.ADVERTISING -> beginStart(nowMs, stopGeneration = generation)
    }

    private fun beginStart(nowMs: Long, stopGeneration: Long?): AdvertiserAction {
        restartPending = false
        retryDueAtMs = null
        generation += 1
        state = AdvertiserState.STARTING
        startedAtMs = nowMs
        return AdvertiserAction(
            stopGeneration = stopGeneration,
            startGeneration = generation,
            // Every start is watched: an unanswered one is exactly as dark as
            // a failed one, and much harder to notice.
            watchdogInMs = START_WATCHDOG_MS,
        )
    }

    /** Parks in [AdvertiserState.IDLE] with a re-arm due; returns its delay. */
    private fun armRetry(nowMs: Long): Long {
        state = AdvertiserState.IDLE
        failureStreak += 1
        val delayMs = retryDelayMs(failureStreak)
        retryDueAtMs = nowMs + delayMs
        return delayMs
    }

    /** [RETRY_MIN_DELAY_MS] doubling per consecutive failure, capped. */
    private fun retryDelayMs(streak: Int): Long {
        var delayMs = RETRY_MIN_DELAY_MS
        repeat((streak - 1).coerceAtLeast(0)) {
            if (delayMs >= RETRY_MAX_DELAY_MS) return RETRY_MAX_DELAY_MS
            delayMs *= 2
        }
        return delayMs.coerceAtMost(RETRY_MAX_DELAY_MS)
    }

    /**
     * A monotonic clock cannot go backwards, but a caller confusing clocks (or
     * a test) can: treat a negative interval as "past due" rather than as a
     * wait that never ends, since erring towards a redundant restart is the
     * cheap direction.
     */
    private fun elapsedSince(startMs: Long, nowMs: Long): Long =
        (nowMs - startMs).let { if (it < 0) Long.MAX_VALUE else it }
}
