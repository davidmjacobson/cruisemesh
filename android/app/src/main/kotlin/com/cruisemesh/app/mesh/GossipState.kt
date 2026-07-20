package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.SeenIds

/**
 * Process-wide holder for the gossip [SeenIds] set (DESIGN.md §5.3), following
 * the same singleton pattern as [MeshRouter] and [com.cruisemesh.app.AppStore].
 *
 * One instance is shared by both sides of the mesh:
 *  - [MeshService]'s receive path (`processInboundEnvelope`) checks
 *    [SeenIds.contains] on every inbound envelope's `msg_id` to decide
 *    whether to process it at all, then calls [SeenIds.record] only once
 *    that envelope reaches a terminal handled state (consumed, carried, or
 *    expired-drop). This check-then-record split is DTN D4: recording up
 *    front via [SeenIds.checkAndRecord] would poison the `msg_id` even if
 *    the subsequent durable handling then failed (e.g. disk-full), dropping
 *    every future copy of that envelope as a duplicate for the rest of the
 *    process lifetime. See `processInboundEnvelope`'s KDoc for the full
 *    reasoning, including why this is safe against flood re-ingestion loops.
 *  - the outgoing send paths ([com.cruisemesh.app.chat.MeshSender] and
 *    [MeshService.sealEnvelopeFrame]) call [SeenIds.record] on the `msg_id`
 *    they author, so a relay flooding our own message back to us doesn't look
 *    "foreign" (we can't open our own sealed box, DESIGN.md §6.3) and get
 *    re-relayed. [SeenIds.checkAndRecord] remains available for callers like
 *    this that have no "did handling fail" question to ask.
 *
 * The Rust [SeenIds] object is internally synchronized, so each individual
 * [SeenIds.contains]/[SeenIds.record] call needs no lock of its own -- but
 * that does NOT make the check-then-record *sequence* atomic, and
 * `processInboundEnvelope` is called concurrently from up to four
 * receive-path threads (central-GATT binder, peripheral-GATT binder,
 * LanTransport's `connectionExecutor`, the relay-sync thread). Two threads
 * can both observe "not seen yet" before either records it. [MeshService]
 * closes that window with a separate atomic in-flight claim
 * ([InboundEnvelopeAdmission], FA5) taken before the `contains` check and
 * released only once handling reaches a terminal state -- see
 * [InboundEnvelopeAdmission]'s KDoc for the full model. [seenIds] itself
 * intentionally outlives any single [MeshService] start/stop cycle: dedupe
 * memory should survive a mesh restart within one process.
 */
object GossipState {
    val seenIds: SeenIds by lazy { SeenIds() }
}
