package com.cruisemesh.app.mesh

/** What [RelaySyncEngine]'s sync thread should do after finishing a pass. */
enum class RelayRerunAction {
    /** A nudge is pending and nothing forbids syncing: run another pass now. */
    RUN_AGAIN,

    /**
     * A nudge is pending but the pass that just finished was rate-limited:
     * hand the nudge to the coalesced Retry-After timer instead of re-running.
     */
    SCHEDULE_RATE_LIMIT_RETRY,

    /** Nothing pending (or syncing is impossible): release the thread. */
    STOP,
}

/**
 * CP2b, rerun edition. The Retry-After gate at the top of
 * `requestRelaySync` only guards the FRONT door: a nudge that arrives while
 * a pass is already in flight just sets the pending flag, and the pending
 * rerun used to start immediately -- ignoring the backoff window the pass
 * it followed had just recorded from a 429. On a phone with a deep carry
 * queue that turned into back-to-back passes under a second apart, each one
 * re-posting a full batch into "too fast", around the clock. The rerun must
 * respect the same window the front door does; iOS is already shaped this
 * way (`finishRelaySync` re-enters through `runRelaySync`'s rate-limit
 * check), so this brings Android to parity.
 *
 * Pure so it can be unit-tested directly; the engine supplies the state and
 * acts on the answer.
 */
fun relayRerunAction(
    pendingRequested: Boolean,
    canSync: Boolean,
    backoffRemainingMs: Long,
): RelayRerunAction = when {
    !pendingRequested || !canSync -> RelayRerunAction.STOP
    backoffRemainingMs > 0 -> RelayRerunAction.SCHEDULE_RATE_LIMIT_RETRY
    else -> RelayRerunAction.RUN_AGAIN
}
