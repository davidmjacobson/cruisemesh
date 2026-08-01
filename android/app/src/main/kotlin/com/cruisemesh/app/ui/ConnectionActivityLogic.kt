package com.cruisemesh.app.ui

import uniffi.cruisemesh_core.PeerConnectionEventKind
import uniffi.cruisemesh_core.PeerConnectionSummary
import uniffi.cruisemesh_core.PeerConnectionTransport

/**
 * Which piece of evidence a connection-history line is reporting, and -- for
 * the two message directions -- WHICH WAY the message went.
 *
 * Declaration order is deliberate: it is the tie-break when two pieces of
 * evidence share a timestamp, most informative first. A message crossing in
 * either direction says more than a bare presence ping, which says more than
 * a link coming up or going down.
 *
 * Kept free of Android and Compose types so it can be unit-tested directly;
 * the screen turns these into localized copy.
 */
enum class PeerEvidence {
    /** A message THEY sent landed on this phone. */
    MESSAGE_RECEIVED,

    /** A message WE sent reached them -- their delivery receipt came back. */
    MESSAGE_DELIVERED,
    PRESENCE_SEEN,
    CONNECTED,
    DISCONNECTED,
}

/** One rendered connection-history line, before it is turned into copy. */
data class PeerStatusLine(
    val evidence: PeerEvidence,
    val transport: PeerConnectionTransport,
    val atMs: Long,
)

/**
 * Picks the newest piece of evidence across every path recorded for one
 * friend.
 *
 * Every timestamp on every row competes on its own: the winner is the single
 * most recent moment, and the transport reported is the one that moment
 * happened on. An earlier version chose a row first and only then looked for
 * a timestamp on it, so a row whose newest field was, say, a stale delivery
 * confirmation could outrank a path that had seen the friend seconds ago.
 *
 * Returns null when there is no evidence at all, so the caller can say so
 * plainly rather than inventing a time.
 */
fun latestPeerStatus(rows: List<PeerConnectionSummary>): PeerStatusLine? =
    rows
        .flatMap { row ->
            listOfNotNull(
                row.lastReceivedAtMs?.let { PeerStatusLine(PeerEvidence.MESSAGE_RECEIVED, row.transport, it) },
                row.lastDeliveredAtMs?.let { PeerStatusLine(PeerEvidence.MESSAGE_DELIVERED, row.transport, it) },
                row.lastSeenAtMs?.let { PeerStatusLine(PeerEvidence.PRESENCE_SEEN, row.transport, it) },
                row.lastConnectedAtMs?.let { PeerStatusLine(PeerEvidence.CONNECTED, row.transport, it) },
                row.lastDisconnectedAtMs?.let { PeerStatusLine(PeerEvidence.DISCONNECTED, row.transport, it) },
            )
        }
        .minWithOrNull(compareByDescending<PeerStatusLine> { it.atMs }.thenBy { it.evidence.ordinal })

/** The evidence a single recorded event represents. */
fun peerEvidenceOf(kind: PeerConnectionEventKind): PeerEvidence = when (kind) {
    PeerConnectionEventKind.CONNECTED -> PeerEvidence.CONNECTED
    PeerConnectionEventKind.DISCONNECTED -> PeerEvidence.DISCONNECTED
    PeerConnectionEventKind.PRESENCE_SEEN -> PeerEvidence.PRESENCE_SEEN
    PeerConnectionEventKind.MESSAGE_DELIVERED -> PeerEvidence.MESSAGE_DELIVERED
    PeerConnectionEventKind.MESSAGE_RECEIVED -> PeerEvidence.MESSAGE_RECEIVED
}
