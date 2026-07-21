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

    @Test
    fun `carried hop_ttl is the authored value minus one`() {
        assertEquals(6u.toUByte(), carriedHopTtl(7u))
        assertEquals(0u.toUByte(), carriedHopTtl(1u))
    }

    @Test
    fun `carried hop_ttl saturates at zero instead of underflowing`() {
        assertEquals(0u.toUByte(), carriedHopTtl(0u))
    }

    @Test
    fun `a one-mule delivery reports one hop taken, not zero`() {
        // Regression for the field bug: a sender authors hop_ttl 7, a single
        // mule carries it (storing carriedHopTtl(7) = 6) and hands it
        // straight to the recipient with that stored value -- the recipient
        // must see this as one hop, not the pre-fix "~0 hops" contradiction
        // ("Arrived via another device" with an apparent 0-hop count).
        val authoredHopTtl = 7u.toUByte()
        val storedByMule = carriedHopTtl(authoredHopTtl)
        assertEquals(1u.toUByte(), arrivalHopsTaken(storedByMule))
    }
}
