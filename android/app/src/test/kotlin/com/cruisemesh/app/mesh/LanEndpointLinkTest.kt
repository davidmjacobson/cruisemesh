package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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

    private val peer = LanManualEndpoint("192.168.86.23", 45_892)

    /**
     * A link must never turn the mesh on. Before this, a tapped
     * `cruisemesh://lan#...` started a stopped mesh and dialed straight away.
     */
    @Test
    fun linkWithMeshOffNeverOffersToConnect() {
        var compared = false
        val decision = decideLanEndpointLink(
            endpoint = peer,
            meshRunning = false,
            localEndpoint = "192.168.86.31:45892",
            sameNetwork = { _, _ -> compared = true; true },
        )

        assertEquals(LanEndpointLinkDecision.MeshOff(peer), decision)
        assertFalse("mesh-off links are not even evaluated", compared)
    }

    @Test
    fun linkWithoutWifiAddressIsRefused() {
        assertEquals(
            LanEndpointLinkDecision.NoWifi(peer),
            decideLanEndpointLink(peer, meshRunning = true, localEndpoint = null),
        )
    }

    /** Uses the real core comparison, not the test seam. */
    @Test
    fun linkToAnotherNetworkIsRefused() {
        val remote = LanManualEndpoint("203.0.113.9", 45_892)
        assertEquals(
            LanEndpointLinkDecision.OtherNetwork(remote),
            decideLanEndpointLink(remote, meshRunning = true, localEndpoint = "192.168.86.31:45892"),
        )
        val neighbourSubnet = LanManualEndpoint("192.168.87.23", 45_892)
        assertEquals(
            LanEndpointLinkDecision.OtherNetwork(neighbourSubnet),
            decideLanEndpointLink(neighbourSubnet, meshRunning = true, localEndpoint = "192.168.86.31:45892"),
        )
        // A hostname is never shown to be on this network.
        val named = LanManualEndpoint("example.com", 45_892)
        assertEquals(
            LanEndpointLinkDecision.OtherNetwork(named),
            decideLanEndpointLink(named, meshRunning = true, localEndpoint = "192.168.86.31:45892"),
        )
    }

    @Test
    fun linkOnThisWifiAsksBeforeConnecting() {
        assertEquals(
            LanEndpointLinkDecision.Confirm(peer),
            decideLanEndpointLink(peer, meshRunning = true, localEndpoint = "192.168.86.31:45892"),
        )
        val v6Peer = LanManualEndpoint("2001:db8:1:2::23", 45_892)
        assertEquals(
            LanEndpointLinkDecision.Confirm(v6Peer),
            decideLanEndpointLink(v6Peer, meshRunning = true, localEndpoint = "[2001:db8:1:2::31]:45892"),
        )
    }
}
