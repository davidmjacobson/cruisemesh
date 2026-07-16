package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test

class MessageArrivalMetadataTest {
    @Test
    fun `relay source wins regardless of BLE peer identity`() {
        assertEquals(ARRIVAL_TRANSPORT_RELAY, arrivalTransport(fromRelay = true, linkPeerMatchesSender = true))
    }

    @Test
    fun `matching BLE peer is direct and a different peer is muled`() {
        assertEquals(ARRIVAL_TRANSPORT_BLE_DIRECT, arrivalTransport(fromRelay = false, linkPeerMatchesSender = true))
        assertEquals(ARRIVAL_TRANSPORT_BLE_MULED, arrivalTransport(fromRelay = false, linkPeerMatchesSender = false))
    }

    @Test
    fun `LAN distinguishes direct sender from a LAN mule`() {
        assertEquals(
            ARRIVAL_TRANSPORT_LAN_DIRECT,
            arrivalTransport(
                fromRelay = false,
                linkPeerMatchesSender = true,
                linkTransport = MeshRouterState.Transport.LAN,
            ),
        )
        assertEquals(
            ARRIVAL_TRANSPORT_LAN_MULED,
            arrivalTransport(
                fromRelay = false,
                linkPeerMatchesSender = false,
                linkTransport = MeshRouterState.Transport.LAN,
            ),
        )
    }

    @Test
    fun `hop count is derived from the original budget and safely clamped`() {
        assertEquals(0u.toUByte(), arrivalHopsTaken(7u))
        assertEquals(2u.toUByte(), arrivalHopsTaken(5u))
        assertEquals(0u.toUByte(), arrivalHopsTaken(9u))
    }
}
