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
 * decision. Both fields are generation numbers, never callbacks: the binder
 * owns the mapping from generation to the one `AdvertiseCallback` instance it
 * registered for that generation, which is the whole point (see the class doc
 * of [BleAdvertiserStateMachine]).
 *
 * When both are set, the stop happens first and the start immediately after.
 */
data class AdvertiserAction(
    val stopGeneration: Long? = null,
    val startGeneration: Long? = null,
) {
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
 * All methods are `@Synchronized`: advertise callbacks arrive on binder
 * threads. This is a leaf monitor -- it never calls out -- so it cannot
 * deadlock with [BlePeripheral]'s own locks.
 */
class BleAdvertiserStateMachine(initialMode: RadioDutyMode = RadioDutyMode.LOW_POWER) {

    private var state = AdvertiserState.IDLE
    private var generation = 0L
    private var desiredMode = initialMode

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
     * (every link teardown calls this) from thrashing the advertiser.
     */
    @Synchronized
    fun onStartRequested(): AdvertiserAction = when (state) {
        AdvertiserState.IDLE -> beginStart(stopGeneration = null)
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
    fun onConnectRestartRequested(): AdvertiserAction = forceRestart()

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
    fun onDutyModeRequested(mode: RadioDutyMode): AdvertiserAction {
        if (mode == desiredMode) return AdvertiserAction.NONE
        desiredMode = mode
        return when (state) {
            // A policy tick is not a request to advertise: if the advertiser
            // is down, only the mode is recorded, exactly as before.
            AdvertiserState.IDLE -> AdvertiserAction.NONE
            AdvertiserState.STARTING, AdvertiserState.ADVERTISING -> forceRestart()
        }
    }

    /**
     * The current generation's `startAdvertising` succeeded. Returns the
     * follow-up restart if one was queued behind it.
     */
    @Synchronized
    fun onStartSucceeded(callbackGeneration: Long): AdvertiserAction {
        if (!acceptsResultFor(callbackGeneration)) return AdvertiserAction.NONE
        state = AdvertiserState.ADVERTISING
        return if (restartPending) forceRestart() else AdvertiserAction.NONE
    }

    /**
     * The current generation's `startAdvertising` failed. We are not
     * advertising -- including for `ADVERTISE_FAILED_ALREADY_STARTED`, which a
     * fresh callback per start makes impossible by construction and which the
     * binder therefore logs as unexpected. Mapping that code to "advertising"
     * is precisely how this class's first bug stayed invisible.
     *
     * No retry is scheduled here: the next start request (a link teardown, a
     * central connect, a duty-mode change, or a fresh [BlePeripheral.start])
     * finds [AdvertiserState.IDLE] and tries again with a new generation.
     */
    @Synchronized
    fun onStartFailed(callbackGeneration: Long): AdvertiserAction {
        if (!acceptsResultFor(callbackGeneration)) return AdvertiserAction.NONE
        state = AdvertiserState.IDLE
        restartPending = false
        return AdvertiserAction.NONE
    }

    /**
     * Full stop (the peripheral role is going away). Retires the current
     * generation whether or not its start ever reported, so a callback still
     * in flight lands as stale and cannot resurrect a dead advertiser.
     */
    @Synchronized
    fun onStopRequested(): AdvertiserAction {
        restartPending = false
        val stopGeneration = if (state == AdvertiserState.IDLE) null else generation
        // Retire unconditionally: the point is that nothing tagged with the
        // old generation is ever accepted again.
        generation += 1
        state = AdvertiserState.IDLE
        return AdvertiserAction(stopGeneration = stopGeneration)
    }

    private fun forceRestart(): AdvertiserAction = when (state) {
        AdvertiserState.IDLE -> beginStart(stopGeneration = null)
        AdvertiserState.STARTING -> {
            restartPending = true
            AdvertiserAction.NONE
        }
        AdvertiserState.ADVERTISING -> beginStart(stopGeneration = generation)
    }

    private fun beginStart(stopGeneration: Long?): AdvertiserAction {
        restartPending = false
        generation += 1
        state = AdvertiserState.STARTING
        return AdvertiserAction(stopGeneration = stopGeneration, startGeneration = generation)
    }
}
