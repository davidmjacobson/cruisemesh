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
