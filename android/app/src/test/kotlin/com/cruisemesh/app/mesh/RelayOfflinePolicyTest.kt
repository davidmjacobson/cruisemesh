package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test

class RelayOfflinePolicyTest {
    @Test
    fun `no relay configured anywhere stays quiet when the internet goes`() {
        // The phone this PR exists for: nearby delivery is the whole
        // arrangement and it is still working, so losing internet is not news.
        assertEquals(RelayHealth.NoConfig, offlineRelayHealth(anyRelayConfigKnown = false))
    }

    @Test
    fun `a phone that has a relay to lose still hears about it`() {
        assertEquals(RelayHealth.NoInternet, offlineRelayHealth(anyRelayConfigKnown = true))
    }

    @Test
    fun `before the first config sweep we do not know, so we do not quiet`() {
        assertEquals(RelayHealth.NoInternet, offlineRelayHealth(anyRelayConfigKnown = null))
    }
}
