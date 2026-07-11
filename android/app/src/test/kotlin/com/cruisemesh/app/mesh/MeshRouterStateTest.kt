package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
        state.onConnected("CENTRAL-LINK", MeshRouterState.Transport.CENTRAL)
        state.onHello("CENTRAL-LINK", alice)
        state.onConnected("PERIPHERAL-LINK", MeshRouterState.Transport.PERIPHERAL)
        state.onHello("PERIPHERAL-LINK", alice)

        // Either link is a valid route; routeFor just needs to find one.
        val route = state.routeFor(alice)
        assert(route == (MeshRouterState.Transport.CENTRAL to "CENTRAL-LINK") ||
            route == (MeshRouterState.Transport.PERIPHERAL to "PERIPHERAL-LINK"))

        // Dropping one link leaves the other still routable to the same userId.
        state.onDisconnected("CENTRAL-LINK")
        assertEquals(MeshRouterState.Transport.PERIPHERAL to "PERIPHERAL-LINK", state.routeFor(alice))
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
    fun `helloedUserIds collapses the same peer's dual-role links into one entry`() {
        val state = MeshRouterState()
        val alice = userId(1)
        state.onConnected("CENTRAL-LINK", MeshRouterState.Transport.CENTRAL)
        state.onHello("CENTRAL-LINK", alice)
        state.onConnected("PERIPHERAL-LINK", MeshRouterState.Transport.PERIPHERAL)
        state.onHello("PERIPHERAL-LINK", alice)

        assertEquals(setOf(com.cruisemesh.app.chat.UserIdHex.encode(alice)), state.helloedUserIds())

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
}
