package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
