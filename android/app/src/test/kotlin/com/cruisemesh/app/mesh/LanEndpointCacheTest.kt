package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LanEndpointCacheTest {
    @Test
    fun manualEndpointDisplayHandlesIpv4AndIpv6() {
        assertEquals("10.0.0.2:45892", LanManualEndpoint("10.0.0.2", 45_892).display)
        assertEquals("[fe80::1234]:45892", LanManualEndpoint("fe80::1234", 45_892).display)
    }

    @Test
    fun malformedManualEndpointsRemainRejected() {
        assertNull(parseLanManualEndpoint("10.0.0.2:0", 45_892))
        assertNull(parseLanManualEndpoint("bad host", 45_892))
    }

    @Test
    fun aCachedHostFromAnOlderBuildIsDroppedUnlessItIsALocalAddress() {
        val savedAt = 1_000L
        val now = 2_000L
        // What older builds could have stored: the cache took any string.
        assertFalse(cachedLanEndpointIsDialable("phone.local", savedAt, now))
        assertFalse(cachedLanEndpointIsDialable("cruisemesh.app", savedAt, now))
        assertFalse(cachedLanEndpointIsDialable("8.8.8.8", savedAt, now))
        assertFalse(cachedLanEndpointIsDialable("", savedAt, now))
        // A real cached LAN address still works, and still expires.
        assertTrue(cachedLanEndpointIsDialable("10.0.0.7", savedAt, now))
        assertTrue(cachedLanEndpointIsDialable("fe80::1%wlan0", savedAt, now))
        assertFalse(
            cachedLanEndpointIsDialable("10.0.0.7", savedAt, savedAt + 7 * 24 * 60 * 60_000 + 1),
        )
    }

    @Test
    fun networkFingerprintUsesTheSharedIpv4Slash24() {
        assertEquals(
            "NcJ68sf-sL-VO63PUTnngg==",
            lanNetworkIdForIpv4("10.154.189.58"),
        )
        assertEquals(
            "NcJ68sf-sL-VO63PUTnngg==",
            lanNetworkIdForIpv4("10.154.189.201"),
        )
        assertNull(lanNetworkIdForIpv4("not-an-ip"))
    }
}
