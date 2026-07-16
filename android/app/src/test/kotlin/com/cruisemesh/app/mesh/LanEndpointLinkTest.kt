package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class LanEndpointLinkTest {
    @Test
    fun endpointQrLinkRoundTripsIpv4AndIpv6() {
        for (endpoint in listOf(
            LanManualEndpoint("10.154.189.58", 45_892),
            LanManualEndpoint("fe80::1234", 45_892),
        )) {
            val fragment = lanEndpointLink(endpoint).substringAfter('#')
            assertEquals(endpoint, parseLanEndpointLink(fragment))
        }
    }

    @Test
    fun endpointQrLinkRejectsUnknownOrMalformedContent() {
        assertNull(parseLanEndpointLink(null))
        assertNull(parseLanEndpointLink("CMFRIEND1:anything"))
        assertNull(parseLanEndpointLink("CMLAN1:not-base64:0"))
    }
}
