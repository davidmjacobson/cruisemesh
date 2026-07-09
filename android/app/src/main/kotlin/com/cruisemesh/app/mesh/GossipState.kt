package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.SeenIds

/**
 * Process-wide holder for the gossip [SeenIds] set (DESIGN.md §5.3), following
 * the same singleton pattern as [MeshRouter] and [com.cruisemesh.app.AppStore].
 *
 * One instance is shared by both sides of the mesh:
 *  - [MeshService]'s receive path calls [SeenIds.checkAndRecord] on every
 *    inbound envelope's `msg_id` to forward each envelope at most once and
 *    drop frames that loop back over the mesh's redundant links.
 *  - the outgoing send paths ([com.cruisemesh.app.chat.MeshSender] and
 *    [MeshService.sealEnvelopeFrame]) call [SeenIds.record] on the `msg_id`
 *    they author, so a relay flooding our own message back to us doesn't look
 *    "foreign" (we can't open our own sealed box, DESIGN.md §6.3) and get
 *    re-relayed.
 *
 * The Rust [SeenIds] object is internally synchronized, so this needs no lock
 * of its own. It intentionally outlives any single [MeshService] start/stop
 * cycle: dedupe memory should survive a mesh restart within one process.
 */
object GossipState {
    val seenIds: SeenIds by lazy { SeenIds() }
}
