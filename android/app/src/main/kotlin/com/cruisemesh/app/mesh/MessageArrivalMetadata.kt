package com.cruisemesh.app.mesh

internal const val ARRIVAL_TRANSPORT_BLE_DIRECT: UByte = 0u
internal const val ARRIVAL_TRANSPORT_BLE_MULED: UByte = 1u
internal const val ARRIVAL_TRANSPORT_RELAY: UByte = 2u
internal const val ARRIVAL_TRANSPORT_LAN_DIRECT: UByte = 3u
internal const val ARRIVAL_TRANSPORT_LAN_MULED: UByte = 4u

internal fun arrivalTransport(
    fromRelay: Boolean,
    linkPeerMatchesSender: Boolean,
    linkTransport: MeshRouterState.Transport? = null,
): UByte = when {
    fromRelay -> ARRIVAL_TRANSPORT_RELAY
    linkTransport == MeshRouterState.Transport.LAN && linkPeerMatchesSender -> ARRIVAL_TRANSPORT_LAN_DIRECT
    linkTransport == MeshRouterState.Transport.LAN -> ARRIVAL_TRANSPORT_LAN_MULED
    linkPeerMatchesSender -> ARRIVAL_TRANSPORT_BLE_DIRECT
    else -> ARRIVAL_TRANSPORT_BLE_MULED
}

internal fun arrivalHopsTaken(
    receivedHopTtl: UByte,
    initialHopTtl: UByte = 7u,
): UByte = (initialHopTtl.toInt() - receivedHopTtl.toInt())
    .coerceIn(0, initialHopTtl.toInt())
    .toUByte()

/**
 * The `hop_ttl` value to persist for a foreign envelope entering the carry
 * queue (DESIGN.md §5.3 store-and-forward), one leg of the sender-authored
 * budget consumed by this device acting as a mule. The flood/relay path
 * ([MeshService.relayForeignEnvelope]) already decrements on every re-flood;
 * before this, the carry path stored `envelope.hopTtl` verbatim at every
 * stage (enqueue, drain, re-flood), so [arrivalHopsTaken] under-counted a
 * pure carry hand-off by exactly the muled leg -- a single-mule delivery
 * showed "~0 hops" instead of "~1 hop". Decrementing once here, at carry
 * enqueue time, keeps every downstream consumer (drain, re-flood, digest
 * paths) consistent without touching them, since they all forward the
 * already-decremented stored value.
 *
 * Saturating: `hop_ttl == 0` (this node is already the final carrier -- see
 * [MeshService.relayForeignEnvelope]'s "hop budget exhausted" comment) stays
 * `0` rather than underflowing. Carry/drop eligibility for a zero-TTL
 * envelope is unchanged by this function; it only affects the stored value
 * for envelopes that do get carried.
 */
internal fun carriedHopTtl(authoredHopTtl: UByte): UByte =
    (authoredHopTtl.toInt() - 1).coerceAtLeast(0).toUByte()
