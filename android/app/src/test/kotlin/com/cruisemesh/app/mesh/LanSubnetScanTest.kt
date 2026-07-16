package com.cruisemesh.app.mesh

import java.net.Inet4Address
import java.net.InetAddress
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class LanSubnetScanTest {
    @Test
    fun scanCandidatesStayInsideOneSlash24AndExcludeSelf() {
        val local = InetAddress.getByName("10.154.189.58") as Inet4Address
        val hosts = subnet24Hosts(local).map { it.hostAddress }

        assertEquals(253, hosts.size)
        assertFalse("10.154.189.58" in hosts)
        assertFalse("10.154.189.0" in hosts)
        assertFalse("10.154.189.255" in hosts)
        assertEquals("10.154.189.1", hosts.first())
        assertEquals("10.154.189.254", hosts.last())
    }
}
