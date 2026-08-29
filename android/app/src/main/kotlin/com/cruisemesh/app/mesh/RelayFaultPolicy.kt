package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreRelayFault
import uniffi.cruisemesh_core.CoreRelayPassHealth
import uniffi.cruisemesh_core.coreRelayPassHealth
import uniffi.cruisemesh_core.coreWorseRelayFault

/**
 * The shell half of the relay pass health fold.
 *
 * Everything that decides anything now lives in the core: the HTTP-shape ->
 * fault classification and the ranking in `core/src/relay_status.rs`, and the
 * worst-of fold plus the "what health did this pass earn" answer in
 * `core/src/session/relay_policy.rs`. What is left here is projection --
 * attaching this shell's clock reading and choosing this shell's display type
 * -- because [RelayHealth] carries a timestamp the core has no business
 * inventing and the core carries no display type. iOS does the same mapping in
 * MeshConnectivityStatus.swift.
 *
 * The reasoning behind the precedence (why an expired pass beats a successful
 * poll and a suspended one does not) lives with the policy, in the core
 * module's doc comments and in `RATE-01`'s prose. It is deliberately not
 * repeated here: a rationale kept next to a mapping is a rationale that drifts
 * from the rule it explains.
 */

/**
 * Worst-of fold for the faults observed against our OWN saved config during
 * one pass. [CoreRelayFault.OUTAGE] is deliberately never folded in by the
 * caller -- an unstructured failure is what the `ownRelaySucceeded /
 * anyRelaySucceeded` flags already express as [RelayHealth.Failing].
 */
fun worseRelayFault(current: CoreRelayFault?, observed: CoreRelayFault): CoreRelayFault =
    coreWorseRelayFault(current, observed)

/** The [RelayHealth] one completed sync pass earns. */
fun relayHealthAfterSyncPass(
    fault: CoreRelayFault?,
    ownRelaySucceeded: Boolean,
    anyRelaySucceeded: Boolean,
    now: Long,
): RelayHealth = relayHealthFor(coreRelayPassHealth(fault, ownRelaySucceeded, anyRelaySucceeded), now)

/**
 * The same projection, for a pass that folded its own health.
 *
 * `CoreRelayPassSummary` already carries a [CoreRelayPassHealth] the session
 * decided from everything it observed, so the core engine has no flags left
 * for the fold above. Both engines end on one mapping rather than two: a
 * display type and a clock reading are all this shell adds in either case.
 */
fun relayHealthFor(health: CoreRelayPassHealth, now: Long): RelayHealth = when (health) {
    CoreRelayPassHealth.OK -> RelayHealth.Ok(now)
    CoreRelayPassHealth.QUOTA_FULL -> RelayHealth.QuotaFull(now)
    CoreRelayPassHealth.MESSAGE_TOO_LARGE -> RelayHealth.MessageTooLarge(now)
    CoreRelayPassHealth.RATE_LIMITED -> RelayHealth.RateLimited(now)
    CoreRelayPassHealth.EXPIRED -> RelayHealth.Expired(now)
    CoreRelayPassHealth.EXPIRED_READ_ONLY -> RelayHealth.ExpiredReadOnly(now)
    CoreRelayPassHealth.SUSPENDED -> RelayHealth.Suspended(now)
    CoreRelayPassHealth.TOKEN_REJECTED -> RelayHealth.TokenRejected(now)
    CoreRelayPassHealth.FAILING -> RelayHealth.Failing(now)
}
