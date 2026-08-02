package com.cruisemesh.app.mesh

import android.os.Build
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.lanDefaultTcpPort
import uniffi.cruisemesh_core.lanServiceType

class LanTransportTest {
    private fun contact(userByte: Int, agreeByte: Int) = Contact(
        userId = ByteArray(16) { userByte.toByte() },
        name = "Peer $userByte",
        signPk = ByteArray(32) { (userByte + 1).toByte() },
        agreePk = ByteArray(32) { agreeByte.toByte() },
        relayUrl = null,
        relayToken = null,
    )

    @Test
    fun `default port is a high IANA user port`() {
        assertEquals(45_892, lanDefaultTcpPort().toInt())
        assertTrue(lanDefaultTcpPort().toInt() in 1_024..49_151)
    }

    @Test
    fun `Android and Bonjour service type spelling variants match`() {
        assertTrue(sameLanServiceType("_cruisemesh._tcp"))
        assertTrue(sameLanServiceType("_cruisemesh._tcp."))
        assertEquals("_cruisemesh._tcp.", lanServiceType())
    }

    @Test
    fun `discovery tokens elect exactly one connection initiator`() {
        assertTrue(shouldInitiateLanConnection("0011", "aabb"))
        assertTrue(!shouldInitiateLanConnection("aabb", "0011"))
        assertTrue(!shouldInitiateLanConnection("aabb", "aabb"))
    }

    @Test
    fun `a crowded network resolves the peers past the live-callback cap`() {
        // Ship Wi-Fi advertises far more services than there are callback
        // slots. The peers found once the cap fills must degrade to the
        // one-shot resolve, not be dropped for the whole Wi-Fi session.
        val routes = (0 until 12).map { live ->
            lanServiceRoute(
                sdkInt = Build.VERSION_CODES.UPSIDE_DOWN_CAKE,
                liveServiceInfoCallbacks = live,
                maxServiceInfoCallbacks = 8,
            )
        }
        assertEquals(List(8) { LanServiceRoute.LIVE_CALLBACK }, routes.take(8))
        assertEquals(List(4) { LanServiceRoute.ONE_SHOT_RESOLVE }, routes.drop(8))
    }

    @Test
    fun `before Android 14 every LAN service takes the one-shot resolve`() {
        assertEquals(
            LanServiceRoute.ONE_SHOT_RESOLVE,
            lanServiceRoute(
                sdkInt = Build.VERSION_CODES.TIRAMISU,
                liveServiceInfoCallbacks = 0,
                maxServiceInfoCallbacks = 8,
            ),
        )
    }

    @Test
    fun `Noise static key resolves only an accepted contact`() {
        val alice = contact(1, 7)
        val bob = contact(2, 8)

        assertArrayEquals(
            bob.userId,
            trustedLanPeerUserId(listOf(alice, bob), bob.agreePk),
        )
        assertNull(trustedLanPeerUserId(listOf(alice, bob), ByteArray(32) { 9 }))
    }

    @Test
    fun `manual endpoint accepts an address with the default or explicit port`() {
        assertEquals(
            LanManualEndpoint("10.154.189.58", 45_892),
            parseLanManualEndpoint("10.154.189.58", 45_892),
        )
        assertEquals(
            LanManualEndpoint("10.154.189.58", 46_000),
            parseLanManualEndpoint("10.154.189.58:46000", 45_892),
        )
        assertEquals(
            LanManualEndpoint("fe80::1234", 45_892),
            parseLanManualEndpoint("[fe80::1234]", 45_892),
        )
    }

    @Test
    fun `manual endpoint rejects malformed or out-of-range ports`() {
        assertNull(parseLanManualEndpoint("", 45_892))
        assertNull(parseLanManualEndpoint("10.0.0.2:", 45_892))
        assertNull(parseLanManualEndpoint("10.0.0.2:not-a-port", 45_892))
        assertNull(parseLanManualEndpoint("10.0.0.2:70000", 45_892))
    }

    @Test
    fun `automatic subnet fallback runs only while LAN discovery is idle`() {
        assertTrue(shouldRunAutomaticLanScan(0, 0, 0, 0))
        assertTrue(!shouldRunAutomaticLanScan(1, 0, 0, 0))
        assertTrue(!shouldRunAutomaticLanScan(0, 1, 0, 0))
        assertTrue(!shouldRunAutomaticLanScan(0, 0, 12, 0))
    }

    @Test
    fun `automatic subnet fallback gate rejects when every busy signal is set`() {
        assertTrue(!shouldRunAutomaticLanScan(2, 3, 41, 0))
    }

    @Test
    fun `automatic subnet fallback gate treats one remaining scan host as busy`() {
        assertTrue(!shouldRunAutomaticLanScan(0, 0, 1, 0))
    }

    @Test
    fun `an unlinked LAN-capable contact keeps the sweep gate open despite live links`() {
        // One connected family member must not stop discovery of the rest.
        assertTrue(shouldRunAutomaticLanScan(1, 0, 0, 1))
        assertTrue(shouldRunAutomaticLanScan(3, 0, 0, 2))
        // But in-flight work still defers, links or not.
        assertTrue(!shouldRunAutomaticLanScan(1, 1, 0, 1))
        assertTrue(!shouldRunAutomaticLanScan(1, 0, 7, 1))
        // Everyone capable is linked: nothing left to sweep for.
        assertTrue(!shouldRunAutomaticLanScan(1, 0, 0, 0))
    }

    @Test
    fun `pending outbound attempts count only keys without an authenticated link`() {
        assertEquals(0, pendingLanOutboundAttempts(emptySet(), emptySet()))
        assertEquals(
            2,
            pendingLanOutboundAttempts(setOf("scan:10.0.0.2", "scan:10.0.0.3"), emptySet()),
        )
        assertEquals(
            1,
            pendingLanOutboundAttempts(
                setOf("scan:10.0.0.2", "scan:10.0.0.3"),
                setOf("scan:10.0.0.2"),
            ),
        )
        // A stale authenticated key with no matching attempt (a connection
        // still winding down after the network dropped) cannot push the
        // count negative and wedge the scan gate.
        assertEquals(0, pendingLanOutboundAttempts(emptySet(), setOf("scan:10.0.0.2")))
        assertTrue(
            shouldRunAutomaticLanScan(
                0,
                pendingLanOutboundAttempts(emptySet(), setOf("scan:10.0.0.2")),
                0,
                0,
            ),
        )
    }

    /** The outbound bookkeeping LanTransport keeps for the scan gate. */
    private class OutboundLinks {
        private val dialled = mutableSetOf<String>()
        private val authenticated = mutableSetOf<String>()

        fun dial(key: String) { dialled += key }
        fun authenticate(key: String) { authenticated += key }

        /** Per-connection cleanup, which may land after a teardown. */
        fun connectionFinished(key: String) {
            dialled -= key
            authenticated -= key
        }

        /** Wi-Fi dropped: per-network state is dropped and sockets closed. */
        fun networkTornDown() {
            dialled.clear()
            authenticated.clear()
        }

        fun pending(): Int = pendingLanOutboundAttempts(dialled, authenticated)
    }

    @Test
    fun `losing Wi-Fi with live links leaves automatic scanning armed on the next join`() {
        val links = OutboundLinks()
        links.dial("cache:friend:10.0.0.2")
        links.authenticate("cache:friend:10.0.0.2")
        links.dial("scan:10.0.0.3")
        links.authenticate("scan:10.0.0.3")
        assertEquals(0, links.pending())

        // A Wi-Fi roam tears the session down while both links are live, and
        // the reader threads only notice their closed sockets afterwards.
        links.networkTornDown()
        links.connectionFinished("cache:friend:10.0.0.2")
        links.connectionFinished("scan:10.0.0.3")

        // Joining the next network: nothing is in flight, so the periodic
        // check must be free to sweep again.
        assertEquals(0, links.pending())
        assertTrue(shouldRunAutomaticLanScan(0, links.pending(), 0, 0))

        // And the gate still defers while a fresh attempt really is pending.
        links.dial("scan:10.1.0.4")
        assertEquals(1, links.pending())
        assertTrue(!shouldRunAutomaticLanScan(0, links.pending(), 0, 0))
    }

    @Test
    fun `automatic subnet fallback gate never reads a negative count as busy`() {
        assertTrue(shouldRunAutomaticLanScan(0, -3, 0, 0))
        assertTrue(shouldRunAutomaticLanScan(0, 0, -1, 0))
    }

    @Test
    fun `authenticated scan endpoints are retained but unrelated TCP services are not`() {
        assertTrue(shouldRetainLanReconnectTarget("scan:10.0.0.2", wasAuthenticated = true))
        assertTrue(!shouldRetainLanReconnectTarget("scan:10.0.0.3", wasAuthenticated = false))
        assertTrue(shouldRetainLanReconnectTarget("manual:10.0.0.4", wasAuthenticated = false))
        assertTrue(shouldRetainLanReconnectTarget("cache:friend:10.0.0.5", wasAuthenticated = false))
    }

    @Test
    fun `reconnect target retention only special-cases the scan colon prefix`() {
        // "scanner:" starts with "scan" but not the "scan:" service-key prefix
        // this function actually gates on; it must not be swept up as noise.
        assertTrue(shouldRetainLanReconnectTarget("scanner:10.0.0.6", wasAuthenticated = false))
        assertTrue(!shouldRetainLanReconnectTarget("scan:", wasAuthenticated = false))
        assertTrue(shouldRetainLanReconnectTarget("scan:", wasAuthenticated = true))
    }
}
