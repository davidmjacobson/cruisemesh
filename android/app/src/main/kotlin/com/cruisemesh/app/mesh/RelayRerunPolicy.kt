package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreRelayRerunAction
import uniffi.cruisemesh_core.coreRelayRerunAction

/**
 * What [RelaySyncEngine]'s sync thread should do after finishing a pass.
 *
 * The decision itself lives in the core
 * (`core/src/session/relay_policy.rs::core_relay_rerun_action`); this is the
 * core enum under the name the engine already uses, so the `when` at the call
 * site is unchanged.
 */
typealias RelayRerunAction = CoreRelayRerunAction

/**
 * `RATE-01`'s second clause: a pending nudge may not bypass the quiet window.
 *
 * The Retry-After gate at the top of `requestRelaySync` only guards the FRONT
 * door. A nudge that arrives while a pass is already in flight just sets the
 * pending flag, and the pending rerun used to start immediately -- ignoring
 * the backoff window the pass it followed had just recorded from a 429. On a
 * phone with a deep carry queue that turned into back-to-back passes under a
 * second apart, each one re-posting a full batch into "too fast", around the
 * clock.
 *
 * The nudge is never lost: [RelayRerunAction.SCHEDULE_RATE_LIMIT_RETRY] means
 * it becomes the coalesced retry at the window's end, which is also why
 * several nudges inside one window cost one pass rather than one pass each.
 *
 * Pure delegation: no constant, no comparison, no branch of its own. iOS calls
 * the same core function from `finishRelaySync`.
 */
fun relayRerunAction(
    pendingRequested: Boolean,
    canSync: Boolean,
    backoffRemainingMs: Long,
): RelayRerunAction = coreRelayRerunAction(pendingRequested, canSync, backoffRemainingMs)
