package com.cruisemesh.app.mesh

import android.os.Build
import java.net.InetSocketAddress
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.lanDefaultTcpPort
import uniffi.cruisemesh_core.lanHostsShareLocalNetwork
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
        assertTrue(ownLanStaticKeyMatches(alice.agreePk, alice.agreePk.copyOf()))
        assertTrue(!ownLanStaticKeyMatches(alice.agreePk, bob.agreePk))
    }

    @Test
    fun `own-id HELLO records a clone only when the session key is ours`() {
        val own = contact(1, 7)
        val other = contact(2, 8)

        // A LAN link authenticated as some other contact can still send a
        // HELLO naming our user id. The user id is only a claim; the session
        // key is the proof, so this one is not recorded.
        assertTrue(
            !ownIdentityHelloIsAuthenticated(
                isLanLink = true,
                ownAgreePk = own.agreePk,
                sessionRemoteStaticKey = other.agreePk,
            ),
        )
        // Same on a link whose session key is not available at all.
        assertTrue(
            !ownIdentityHelloIsAuthenticated(
                isLanLink = true,
                ownAgreePk = own.agreePk,
                sessionRemoteStaticKey = null,
            ),
        )
        // A cleartext BLE HELLO never records, whatever it names.
        assertTrue(
            !ownIdentityHelloIsAuthenticated(
                isLanLink = false,
                ownAgreePk = own.agreePk,
                sessionRemoteStaticKey = own.agreePk.copyOf(),
            ),
        )
        // A LAN link that actually holds our own agreement key is the real
        // clone sighting.
        assertTrue(
            ownIdentityHelloIsAuthenticated(
                isLanLink = true,
                ownAgreePk = own.agreePk,
                sessionRemoteStaticKey = own.agreePk.copyOf(),
            ),
        )
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
    fun `a hinted address is tried once and never becomes a reconnect target`() {
        val hintKey = lanHintConnectKey("a1b2c3d4e5f60718")
        assertTrue(isSingleShotLanConnectKey(hintKey))
        // Keys this phone found itself keep retrying: mDNS service names,
        // subnet sweep hits, and manual entry.
        assertTrue(!isSingleShotLanConnectKey("CruiseMesh-abc123._cruisemesh._tcp"))
        assertTrue(!isSingleShotLanConnectKey("scan:10.0.0.2"))
        assertTrue(!isSingleShotLanConnectKey("manual:10.0.0.4:45892"))
        // The hint key stays distinct from the bare instance token so a
        // hint can never take over a discovered peer's retry state.
        assertTrue(!isSingleShotLanConnectKey("a1b2c3d4e5f60718"))
    }

    @Test
    fun `a cached address is a remembered hint and retries no harder than one`() {
        // A cache entry is only ever a hint this phone wrote down, so it
        // carries no better evidence than the hint did. Retrying it on a
        // timer is what turned one stale address into a dial every sixty
        // seconds forever; onLanNetworkReady replays the cache on each Wi-Fi
        // join, so the address still gets an attempt whenever anything about
        // the network could have changed.
        val cachedKey = lanCachedConnectKey("a1b2c3d4e5f60718", "10.0.0.5:45892")
        assertEquals("cache:a1b2c3d4e5f60718:10.0.0.5:45892", cachedKey)
        assertTrue(isSingleShotLanConnectKey(cachedKey))
        assertTrue(isSingleShotLanConnectKey("cache:friend:10.0.0.5:45892"))
        // A key that merely mentions the word is not a cached key.
        assertTrue(!isSingleShotLanConnectKey("scan:10.0.0.2/cache:"))
    }

    @Test
    fun `a hinted address is filed only when it is on this phone's own subnet`() {
        // The field failure: a phone on 192.168.86.0/24 kept a hint for
        // 10.80.209.68 as if it belonged to the network it was on. This is
        // the rule for a *hint*; an endpoint that authenticated is filed on
        // its own authority (MeshService.onLanPeerAuthenticated), because an
        // address that answered is better evidence than a claim about one.
        assertTrue(
            lanHostsShareLocalNetwork(
                localHost = "192.168.86.31",
                candidateHost = "192.168.86.23",
            ),
        )
        assertTrue(
            !lanHostsShareLocalNetwork(
                localHost = "192.168.86.31",
                candidateHost = "10.80.209.68",
            ),
        )
        // Unprovable is treated as "no": names, IPv6 literals, and garbage.
        assertTrue(
            !lanHostsShareLocalNetwork(
                localHost = "192.168.86.31",
                candidateHost = "phone.local",
            ),
        )
        assertTrue(
            !lanHostsShareLocalNetwork(localHost = "192.168.86.31", candidateHost = "fe80::1"),
        )
        assertTrue(!lanHostsShareLocalNetwork(localHost = "", candidateHost = "192.168.86.23"))
        // An IPv6-only Wi-Fi network still has a usable fingerprint, so hints
        // on one are cacheable -- except link-local, which is fe80::/64 on
        // every link there has ever been and therefore proves nothing.
        assertTrue(
            lanHostsShareLocalNetwork(
                localHost = "2001:db8:1:2::31",
                candidateHost = "2001:db8:1:2::23",
            ),
        )
        assertTrue(
            !lanHostsShareLocalNetwork(
                localHost = "2001:db8:1:2::31",
                candidateHost = "2001:db8:1:3::23",
            ),
        )
        assertTrue(
            !lanHostsShareLocalNetwork(localHost = "fe80::1", candidateHost = "fe80::2"),
        )
        assertTrue(!lanHintMayBeCached("192.168.86.31", "192.168.86.31"))
    }

    @Test
    fun `outbound dialing removes self without dropping other resolved addresses`() {
        val self = InetSocketAddress("192.168.86.20", 45_892)
        val peer = InetSocketAddress("192.168.86.23", 45_892)

        assertEquals(
            listOf(peer),
            remoteLanEndpoints("192.168.86.20", listOf(self, peer)),
        )
        assertTrue(remoteLanEndpoints("192.168.86.20", listOf(self)).isEmpty())
        assertEquals(listOf(self, peer), remoteLanEndpoints(null, listOf(self, peer)))
    }

    @Test
    fun `a single-shot address keeps a reconnect target only while it stays proven`() {
        // The point of single-shot is to stop dialing an address nothing ever
        // answered on. An address that completed a Noise handshake is not
        // that address: dropping its reconnect target would leave a working
        // LAN link waiting for the next Wi-Fi join after the access point
        // idles the socket out. So an authenticated close retains the
        // target...
        val cachedKey = lanCachedConnectKey("a1b2c3d4e5f60718", "10.0.0.5:45892")
        val hintKey = lanHintConnectKey("00112233445566778899aabbccddeeff")
        assertTrue(shouldRetainLanReconnectTarget(cachedKey, wasAuthenticated = true))
        assertTrue(shouldRetainLanReconnectTarget(hintKey, wasAuthenticated = true))
        // ...and a close without authentication -- including the retry that
        // proof bought -- retires it again, so one good handshake can never
        // license a permanent background probe.
        assertTrue(!shouldRetainLanReconnectTarget(cachedKey, wasAuthenticated = false))
        assertTrue(!shouldRetainLanReconnectTarget(hintKey, wasAuthenticated = false))
        // A sweep hit that was not a friend is dropped the same way, while
        // evidence this phone can regather itself is kept.
        assertTrue(!shouldRetainLanReconnectTarget("scan:10.0.0.5:45892", wasAuthenticated = false))
        assertTrue(shouldRetainLanReconnectTarget("nsd:friend-phone", wasAuthenticated = false))
        assertTrue(shouldRetainLanReconnectTarget("manual:10.0.0.5:45892", wasAuthenticated = false))
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

    @Test
    fun `a contact last seen on a LAN long ago stops motivating sweeps`() {
        val day = 24 * 60 * 60 * 1_000L
        val now = 100 * day

        // Seen on a LAN within the window: still worth sweeping for.
        assertTrue(lanCapabilityMotivatesScan(now, now))
        assertTrue(lanCapabilityMotivatesScan(now - 13 * day, now))
        // A family member who went ashore two weeks ago does not keep every
        // remaining phone sweeping the subnet forever.
        assertTrue(!lanCapabilityMotivatesScan(now - 14 * day, now))
        assertTrue(!lanCapabilityMotivatesScan(now - 400 * day, now))
        // Never demonstrated LAN support at all (including capability
        // recorded before this timestamp existed).
        assertTrue(!lanCapabilityMotivatesScan(null, now))
        // A clock that moved backwards must not expire a fresh sighting.
        assertTrue(lanCapabilityMotivatesScan(now + day, now))
    }

    @Test
    fun `the sweep gate closes once every capable contact has gone stale`() {
        val now = 50L * 24 * 60 * 60 * 1_000
        val stale = now - 30L * 24 * 60 * 60 * 1_000
        val capable = mapOf("aa" to stale, "bb" to stale)
        val motivating = capable.count { (_, lastSeen) ->
            lanCapabilityMotivatesScan(lastSeen, now)
        }

        assertEquals(0, motivating)
        // A live link plus no motivating contact means no sweep at all.
        assertTrue(!shouldRunAutomaticLanScan(1, 0, 0, motivating))
    }

    @Test
    fun `per-network bookkeeping forgets its oldest key instead of refusing new ones`() {
        val keys = BoundedLanKeySet(limit = 4)
        val forgotten = mutableListOf<String>()

        repeat(4) { assertTrue(keys.claim("token-$it", forgotten::add)) }
        assertEquals(4, keys.size())
        assertEquals(emptyList<String>(), forgotten)

        // A key already claimed is not new work, and costs nothing.
        assertTrue(!keys.claim("token-0", forgotten::add))
        assertEquals(4, keys.size())
        assertEquals(emptyList<String>(), forgotten)

        // At the cap a brand-new key is still accepted -- the OLDEST is
        // forgotten to make room. Refusing instead would let a flood of
        // made-up names lock a real family member out of discovery for the
        // rest of the network join.
        assertTrue(keys.claim("token-4", forgotten::add))
        assertEquals(listOf("token-0"), forgotten)
        assertEquals(4, keys.size())
        assertTrue(!keys.contains("token-0"))
        assertTrue(keys.contains("token-4"))

        // A real peer arriving after a 100-name spray still reads as new.
        repeat(100) { keys.claim("spray-$it") }
        assertEquals(4, keys.size())
        assertTrue(keys.claim("family-phone"))
    }

    @Test
    fun `a removed or cleared bookkeeping key can be claimed again`() {
        val keys = BoundedLanKeySet(limit = 4)

        assertTrue(keys.claim("service-a"))
        assertTrue(!keys.claim("service-a"))
        keys.remove("service-a")
        assertTrue(keys.claim("service-a"))

        keys.clear()
        assertEquals(0, keys.size())
        assertTrue(keys.claim("service-a"))
    }

    @Test
    fun `a sweep probe that cannot open a link still counts an already-linked friend`() {
        // The probe collided with a service key an authenticated link already
        // holds: a healthy link keeps its key for its whole life, so this is
        // how every sweep after the one that linked the family sees them.
        assertTrue(
            lanSweepProbeFoundFriend(
                keyAlreadyAuthenticated = true,
                linkTableFull = false,
                authenticatedLinks = 1,
            ),
        )
        // The link table is full and a friend is on it: the healthiest
        // network there is, not an empty one.
        assertTrue(
            lanSweepProbeFoundFriend(
                keyAlreadyAuthenticated = false,
                linkTableFull = true,
                authenticatedLinks = 2,
            ),
        )
        // A full table of in-flight handshakes to unrelated services proves
        // nothing about friends being here.
        assertTrue(
            !lanSweepProbeFoundFriend(
                keyAlreadyAuthenticated = false,
                linkTableFull = true,
                authenticatedLinks = 0,
            ),
        )
        // Colliding with an attempt that has not authenticated is not a find
        // either, however many other links exist.
        assertTrue(
            !lanSweepProbeFoundFriend(
                keyAlreadyAuthenticated = false,
                linkTableFull = false,
                authenticatedLinks = 3,
            ),
        )
    }

    @Test
    fun `a sweep is only credited with a find while it is still the running sweep`() {
        assertTrue(
            lanSweepCreditApplies(
                sweepGeneration = 7,
                currentGeneration = 7,
                sweepStillRunning = true,
            ),
        )
        // Completed or cancelled: nothing to credit.
        assertTrue(
            !lanSweepCreditApplies(
                sweepGeneration = 7,
                currentGeneration = 7,
                sweepStillRunning = false,
            ),
        )
        // A late handshake from a replaced sweep must not credit the new one.
        assertTrue(
            !lanSweepCreditApplies(
                sweepGeneration = 7,
                currentGeneration = 8,
                sweepStillRunning = true,
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
        // A cached address is unproven evidence in exactly the way a failed
        // sweep hit is -- see the single-shot rule.
        assertTrue(!shouldRetainLanReconnectTarget("cache:friend:10.0.0.5", wasAuthenticated = false))
        assertTrue(shouldRetainLanReconnectTarget("cache:friend:10.0.0.5", wasAuthenticated = true))
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
