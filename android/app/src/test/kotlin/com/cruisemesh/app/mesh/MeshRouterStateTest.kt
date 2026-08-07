package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MeshRouterStateTest {

    private fun userId(byte: Int): ByteArray = ByteArray(16) { byte.toByte() }

    @Test
    fun `an address with no HELLO yet has no known userId`() {
        val state = MeshRouterState()
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        assertNull(state.userIdFor("AA:BB"))
        assertNull(state.routeFor(userId(1)))
    }

    @Test
    fun `HELLO on a connected address makes it routable by userId`() {
        val state = MeshRouterState()
        val alice = userId(1)
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        state.onHello("AA:BB", alice)

        assertEquals(alice.toList(), state.userIdFor("AA:BB")!!.toList())
        assertEquals(MeshRouterState.Transport.CENTRAL to "AA:BB", state.routeFor(alice))
    }

    @Test
    fun `a HELLO for an address that never connected is a no-op`() {
        val state = MeshRouterState()
        state.onHello("AA:BB", userId(1))
        assertNull(state.userIdFor("AA:BB"))
    }

    @Test
    fun `disconnecting forgets the address so it is no longer routable`() {
        val state = MeshRouterState()
        val alice = userId(1)
        state.onConnected("AA:BB", MeshRouterState.Transport.PERIPHERAL)
        state.onHello("AA:BB", alice)
        assertEquals(MeshRouterState.Transport.PERIPHERAL to "AA:BB", state.routeFor(alice))

        state.onDisconnected("AA:BB")

        assertNull(state.routeFor(alice))
        assertNull(state.userIdFor("AA:BB"))
        assertNull(state.transportFor("AA:BB"))
    }

    @Test
    fun `same peer connected via both roles is routable while either link is up`() {
        val state = MeshRouterState()
        val alice = userId(1)
        state.setLocalUserId(userId(0))
        state.onConnected("CENTRAL-LINK", MeshRouterState.Transport.CENTRAL)
        state.onHello("CENTRAL-LINK", alice)
        state.onConnected("PERIPHERAL-LINK", MeshRouterState.Transport.PERIPHERAL)
        state.onHello("PERIPHERAL-LINK", alice)

        assertEquals(MeshRouterState.Transport.CENTRAL to "CENTRAL-LINK", state.routeFor(alice))

        // Dropping one link leaves the other still routable to the same userId.
        state.onDisconnected("CENTRAL-LINK")
        assertEquals(MeshRouterState.Transport.PERIPHERAL to "PERIPHERAL-LINK", state.routeFor(alice))
    }

    @Test
    fun `LAN is preferred when the same peer is also reachable over BLE`() {
        val state = MeshRouterState()
        val alice = userId(1)
        state.onConnected("BLE", MeshRouterState.Transport.CENTRAL)
        state.onHello("BLE", alice)
        state.onConnected("LAN", MeshRouterState.Transport.LAN)
        state.onHello("LAN", alice)

        assertEquals(MeshRouterState.Transport.LAN to "LAN", state.routeFor(alice))

        state.onDisconnected("LAN")
        assertEquals(MeshRouterState.Transport.CENTRAL to "BLE", state.routeFor(alice))
    }

    @Test
    fun `all frame sizes use one elected logical peer route`() {
        val routes = listOf(
            MeshRouterState.Transport.LAN to "LAN",
            MeshRouterState.Transport.CENTRAL to "BLE-1",
            MeshRouterState.Transport.PERIPHERAL to "BLE-2",
        )

        assertEquals(
            listOf(MeshRouterState.Transport.LAN to "LAN"),
            transportSendPlan(routes, frameSize = 512),
        )
        assertEquals(
            listOf(MeshRouterState.Transport.LAN to "LAN"),
            transportSendPlan(routes, frameSize = 64 * 1024),
        )
    }

    @Test
    fun `authenticated mapping cannot be replaced by a conflicting HELLO`() {
        val state = MeshRouterState()
        val alice = userId(1)
        val mallory = userId(9)
        state.onConnected("LAN", MeshRouterState.Transport.LAN)

        assertTrue(state.onHello("LAN", alice))
        assertTrue(!state.onHello("LAN", mallory))
        assertEquals(alice.toList(), state.userIdFor("LAN")!!.toList())
    }

    @Test
    fun `clearing BLE transports preserves a live LAN route`() {
        val state = MeshRouterState()
        state.onConnected("BLE", MeshRouterState.Transport.CENTRAL)
        state.onConnected("LAN", MeshRouterState.Transport.LAN)

        state.clearTransports(
            setOf(
                MeshRouterState.Transport.CENTRAL,
                MeshRouterState.Transport.PERIPHERAL,
            ),
        )

        assertNull(state.transportFor("BLE"))
        assertEquals(MeshRouterState.Transport.LAN, state.transportFor("LAN"))
    }

    @Test
    fun `two different peers never get confused with each other`() {
        val state = MeshRouterState()
        val alice = userId(1)
        val bob = userId(2)
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        state.onHello("AA:BB", alice)
        state.onConnected("CC:DD", MeshRouterState.Transport.PERIPHERAL)
        state.onHello("CC:DD", bob)

        assertEquals(MeshRouterState.Transport.CENTRAL to "AA:BB", state.routeFor(alice))
        assertEquals(MeshRouterState.Transport.PERIPHERAL to "CC:DD", state.routeFor(bob))
    }

    @Test
    fun `transportFor reflects the connected role even before a HELLO arrives`() {
        val state = MeshRouterState()
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        assertEquals(MeshRouterState.Transport.CENTRAL, state.transportFor("AA:BB"))
        assertNull(state.transportFor("NEVER-CONNECTED"))
    }

    @Test
    fun `connectedRoutes lists every live link including ones with no HELLO yet`() {
        val state = MeshRouterState()
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        state.onHello("AA:BB", userId(1))
        state.onConnected("CC:DD", MeshRouterState.Transport.PERIPHERAL) // no HELLO yet

        val routes = state.connectedRoutes().toSet()
        assertEquals(
            setOf(
                MeshRouterState.Transport.CENTRAL to "AA:BB",
                MeshRouterState.Transport.PERIPHERAL to "CC:DD",
            ),
            routes,
        )
    }

    @Test
    fun `connectedRoutes drops a link once it disconnects`() {
        val state = MeshRouterState()
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        state.onConnected("CC:DD", MeshRouterState.Transport.PERIPHERAL)
        state.onDisconnected("AA:BB")

        assertEquals(
            listOf(MeshRouterState.Transport.PERIPHERAL to "CC:DD"),
            state.connectedRoutes(),
        )
    }

    @Test
    fun `identifiedRoutes includes only links that completed HELLO`() {
        val state = MeshRouterState()
        val alice = userId(1)
        state.onConnected("BLE", MeshRouterState.Transport.CENTRAL)
        state.onHello("BLE", alice)
        state.onConnected("UNKNOWN", MeshRouterState.Transport.PERIPHERAL)

        val route = state.identifiedRoutes().single()
        assertEquals(MeshRouterState.Transport.CENTRAL, route.transport)
        assertEquals("BLE", route.address)
        assertEquals(alice.toList(), route.userId.toList())
    }

    @Test
    fun `helloedUserIds collapses the same peer's dual-role links into one entry`() {
        val state = MeshRouterState()
        val alice = userId(1)
        state.onConnected("CENTRAL-LINK", MeshRouterState.Transport.CENTRAL)
        state.onHello("CENTRAL-LINK", alice)
        state.onConnected("PERIPHERAL-LINK", MeshRouterState.Transport.PERIPHERAL)
        state.onHello("PERIPHERAL-LINK", alice)

        assertEquals(setOf(com.cruisemesh.app.chat.UserIdHex.encode(alice)), state.helloedUserIds())
        assertEquals(1, state.connectedUserCount())

        state.onDisconnected("CENTRAL-LINK")
        assertEquals(setOf(com.cruisemesh.app.chat.UserIdHex.encode(alice)), state.helloedUserIds())

        state.onDisconnected("PERIPHERAL-LINK")
        assertEquals(emptySet<String>(), state.helloedUserIds())
    }

    @Test
    fun `helloedUserIds excludes connected addresses that have not HELLO'd yet`() {
        val state = MeshRouterState()
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        assertEquals(emptySet<String>(), state.helloedUserIds())
    }

    @Test
    fun `nearbyTransports maps each HELLO'd peer to its live transport`() {
        val state = MeshRouterState()
        val alice = userId(1)
        val bob = userId(2)
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        state.onHello("AA:BB", alice)
        state.onConnected("CC:DD", MeshRouterState.Transport.LAN)
        state.onHello("CC:DD", bob)

        assertEquals(
            mapOf(
                com.cruisemesh.app.chat.UserIdHex.encode(alice) to MeshRouterState.Transport.CENTRAL,
                com.cruisemesh.app.chat.UserIdHex.encode(bob) to MeshRouterState.Transport.LAN,
            ),
            state.nearbyTransports(),
        )
    }

    @Test
    fun `nearbyTransports excludes connected addresses that have not HELLO'd yet`() {
        val state = MeshRouterState()
        state.onConnected("AA:BB", MeshRouterState.Transport.CENTRAL)
        assertEquals(emptyMap<String, MeshRouterState.Transport>(), state.nearbyTransports())
    }

    // The B5 zombie-header scenario: while a peer is reachable over both Wi-Fi
    // and BLE, nearbyTransports reports LAN (matching routeFor's precedence);
    // the instant the LAN link drops it must flip to BLE so the observing UI
    // stops claiming "Nearby via Wi-Fi" over a dead radio.
    @Test
    fun `nearbyTransports flips LAN to BLE when the Wi-Fi link drops but BLE survives`() {
        val state = MeshRouterState()
        val alice = userId(1)
        state.onConnected("BLE", MeshRouterState.Transport.CENTRAL)
        state.onHello("BLE", alice)
        state.onConnected("LAN", MeshRouterState.Transport.LAN)
        state.onHello("LAN", alice)
        val hex = com.cruisemesh.app.chat.UserIdHex.encode(alice)

        assertEquals(mapOf(hex to MeshRouterState.Transport.LAN), state.nearbyTransports())

        state.onDisconnected("LAN")

        // Peer is still HELLO'd (helloedUserIds unchanged), but the transport
        // must now report BLE -- this is the change the UI observes.
        assertEquals(setOf(hex), state.helloedUserIds())
        assertEquals(mapOf(hex to MeshRouterState.Transport.CENTRAL), state.nearbyTransports())
    }

    @Test
    fun `relay routes collapse duplicate addresses and roles for one authenticated user`() {
        val state = MeshRouterState()
        val alice = userId(1)
        val bob = userId(2)
        state.onConnected("ALICE-CENTRAL", MeshRouterState.Transport.CENTRAL)
        state.onHello("ALICE-CENTRAL", alice)
        state.onConnected("ALICE-PERIPHERAL", MeshRouterState.Transport.PERIPHERAL)
        state.onHello("ALICE-PERIPHERAL", alice)
        state.onConnected("ALICE-LAN", MeshRouterState.Transport.LAN)
        state.onHello("ALICE-LAN", alice)
        state.onConnected("BOB", MeshRouterState.Transport.CENTRAL)
        state.onHello("BOB", bob)
        state.onConnected("UNKNOWN-1", MeshRouterState.Transport.CENTRAL)
        state.onConnected("UNKNOWN-2", MeshRouterState.Transport.PERIPHERAL)

        assertEquals(
            setOf(
                MeshRouterState.Transport.LAN to "ALICE-LAN",
                MeshRouterState.Transport.CENTRAL to "BOB",
                MeshRouterState.Transport.CENTRAL to "UNKNOWN-1",
                MeshRouterState.Transport.PERIPHERAL to "UNKNOWN-2",
            ),
            state.relayRoutes().toSet(),
        )
    }

    @Test
    fun `relay exclusion drops every route back to the arriving logical peer`() {
        val state = MeshRouterState()
        val alice = userId(1)
        val bob = userId(2)
        state.onConnected("ALICE-CENTRAL", MeshRouterState.Transport.CENTRAL)
        state.onHello("ALICE-CENTRAL", alice)
        state.onConnected("ALICE-PERIPHERAL", MeshRouterState.Transport.PERIPHERAL)
        state.onHello("ALICE-PERIPHERAL", alice)
        state.onConnected("BOB", MeshRouterState.Transport.CENTRAL)
        state.onHello("BOB", bob)

        assertEquals(
            listOf(MeshRouterState.Transport.CENTRAL to "BOB"),
            state.relayRoutes(exceptAddress = "ALICE-CENTRAL"),
        )
    }
}
