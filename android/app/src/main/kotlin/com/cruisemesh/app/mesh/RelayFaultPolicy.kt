package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreRelayFault
import uniffi.cruisemesh_core.relayFaultRank

/**
 * CP2b: pure policy for folding the structured relay faults one sync pass
 * observed into the single [RelayHealth] the Shore Pass indicator renders.
 * The classification itself (HTTP status/`code` -> [CoreRelayFault], and
 * which faults self-heal) lives in the core (`core/src/relay_status.rs`);
 * this file only decides which fault wins the pass and how it lands in the
 * shell's health model. No Android imports, unit-tested directly; iOS
 * mirrors it in MeshConnectivityStatus.swift's `RelayHealth.afterSyncPass`.
 */

/**
 * Worst-of fold for the faults observed against our OWN saved config during
 * one pass, using the core's shared ranking so both shells keep the same
 * answer. [CoreRelayFault.OUTAGE] is deliberately never folded in by the
 * caller -- an unstructured failure is what the `ownRelaySucceeded /
 * anyRelaySucceeded` flags already express as [RelayHealth.Failing].
 */
fun worseRelayFault(current: CoreRelayFault?, observed: CoreRelayFault): CoreRelayFault {
    if (current == null) return observed
    return if (relayFaultRank(observed) > relayFaultRank(current)) observed else current
}

/**
 * The [RelayHealth] one completed sync pass earns.
 *
 * The mailbox-level faults (quota, oversized, rate-limited) surface even
 * when polling succeeded: relayd keeps serving fetches while rejecting
 * posts, so before CP2b those rejections vanished into a green check and a
 * silent retry loop.
 *
 * [CoreRelayFault.PASS_EXPIRED] belongs in that same group, for the same
 * reason. For relayd's `FAMILY_EXPIRY_GRACE_MS` -- seven days -- an expired
 * pass keeps fetching and acking so nobody's last messages are stranded
 * mid-cruise, and only POSTs take the 403. So the pass's success flags say
 * "reachable" for a week while every new message is rejected, and folding
 * expiry below them told a paying family their Shore Pass was working
 * when nothing they wrote was leaving the phone.
 *
 * The other two credential faults keep the pre-CP2b precedence, and that is
 * not an oversight: relayd rejects EVERY op for a suspended family and for
 * an unknown token (`authorize_family`), so neither can co-occur with a
 * successful poll at all. Expiry-in-grace is the only credential fault that
 * can, which is why it is the only one that moves.
 */
fun relayHealthAfterSyncPass(
    fault: CoreRelayFault?,
    ownRelaySucceeded: Boolean,
    anyRelaySucceeded: Boolean,
    now: Long,
): RelayHealth {
    when (fault) {
        CoreRelayFault.MAILBOX_FULL -> return RelayHealth.QuotaFull(now)
        CoreRelayFault.MESSAGE_TOO_LARGE -> return RelayHealth.MessageTooLarge(now)
        CoreRelayFault.RATE_LIMITED -> return RelayHealth.RateLimited(now)
        CoreRelayFault.PASS_EXPIRED -> return RelayHealth.Expired(now)
        else -> {}
    }
    if (ownRelaySucceeded && anyRelaySucceeded) return RelayHealth.Ok(now)
    return when (fault) {
        CoreRelayFault.PASS_SUSPENDED -> RelayHealth.Suspended(now)
        CoreRelayFault.TOKEN_REJECTED -> RelayHealth.TokenRejected(now)
        else -> RelayHealth.Failing(now)
    }
}
