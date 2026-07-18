package com.cruisemesh.app.mesh

import java.net.Inet4Address
import java.net.InetAddress
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LanSubnetScanTest {
    @Test
    fun scanCandidatesStayInsideOneSlash24AndExcludeSelf() {
        val local = InetAddress.getByName("10.154.189.58") as Inet4Address
        val hosts = subnetHosts(local, 24).map { it.hostAddress }

        assertEquals(253, hosts.size)
        assertFalse("10.154.189.58" in hosts)
        assertFalse("10.154.189.0" in hosts)
        assertFalse("10.154.189.255" in hosts)
        assertEquals("10.154.189.1", hosts.first())
        assertEquals("10.154.189.254", hosts.last())
    }

    @Test
    fun slash16CoversEveryHostInTheSecondOctetRangeExcludingSelfAndEdges() {
        val local = InetAddress.getByName("10.20.30.40") as Inet4Address
        val hosts = subnetHosts(local, 16).map { it.hostAddress }

        // 2^16 - 2 usable hosts, minus this phone.
        assertEquals(65_533, hosts.size)
        assertFalse("10.20.30.40" in hosts)
        assertFalse("10.20.0.0" in hosts)
        assertFalse("10.20.255.255" in hosts)
        // Spans the whole /16, not just our own /24.
        assertTrue("10.20.0.1" in hosts)
        assertTrue("10.20.255.254" in hosts)
        assertTrue("10.20.99.99" in hosts)
    }

    @Test
    fun networkBroaderThanSlash16IsClampedToASixteenAroundThisAddress() {
        val local = InetAddress.getByName("10.20.30.40") as Inet4Address
        // A /8 must not enumerate 16M hosts -- it clamps to the /16 the phone
        // sits in, so the range stays 10.20.x.x.
        val hosts = subnetHosts(local, 8).map { it.hostAddress }

        assertEquals(65_533, hosts.size)
        assertFalse("10.19.255.254" in hosts)
        assertFalse("10.21.0.1" in hosts)
        assertTrue("10.20.0.1" in hosts)
        assertTrue("10.20.255.254" in hosts)
    }

    @Test
    fun narrowSubnetIsScannedAtItsActualBreadth() {
        val local = InetAddress.getByName("192.168.1.5") as Inet4Address
        val hosts = subnetHosts(local, 30).map { it.hostAddress }

        // 192.168.1.4/30 -> hosts .5 and .6, minus self (.5) leaves one.
        assertEquals(listOf("192.168.1.6"), hosts)
    }

    @Test
    fun effectiveScanPrefixIsClampedToTheSupportedBreadthRange() {
        assertEquals(16, effectiveScanPrefixLength(8))
        assertEquals(16, effectiveScanPrefixLength(16))
        assertEquals(22, effectiveScanPrefixLength(22))
        assertEquals(24, effectiveScanPrefixLength(24))
        assertEquals(30, effectiveScanPrefixLength(32))
    }
}
