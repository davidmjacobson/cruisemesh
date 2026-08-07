package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Shell-side cover for the failover resume debounce -- the coalescing rule
 * itself is pinned in the core's own tests; these assert the shape
 * [MeshService.scheduleFailoverResume] actually depends on.
 */
class FailoverResumeDebounceTest {

    @Test
    fun `the default window outlasts the observed disconnect burst`() {
        // 2026-08-07 capture: one radio event's BLE disconnect callbacks
        // arrived spread over ~240ms. A shorter window would let the resume
        // fan out into a link whose own disconnect is still in flight, which
        // is the whole bug.
        assertTrue(FailoverResumeDebounce().windowMs > 240)
    }

    @Test
    fun `one radio event's worth of disconnects produces exactly one resume`() {
        val debounce = FailoverResumeDebounce(windowMs = 300)
        val peer = "aabbccdd"
        assertEquals(300L, debounce.request(peer, nowMs = 1_000))
        assertNull(debounce.request(peer, nowMs = 1_090))
        assertNull(debounce.request(peer, nowMs = 1_240))
        assertTrue(debounce.isPending(peer))

        debounce.fired(peer)
        assertFalse(debounce.isPending(peer))
        // A genuinely later failover is a new burst.
        assertEquals(300L, debounce.request(peer, nowMs = 5_000))
    }

    @Test
    fun `two peers failing over together each get their own resume`() {
        val debounce = FailoverResumeDebounce(windowMs = 300)
        assertEquals(300L, debounce.request("peer-a", nowMs = 0))
        assertEquals(300L, debounce.request("peer-b", nowMs = 5))
    }

    @Test
    fun `clear drops pending windows so a restarted service is not wedged`() {
        val debounce = FailoverResumeDebounce(windowMs = 300)
        assertEquals(300L, debounce.request("peer", nowMs = 0))
        debounce.clear()
        assertFalse(debounce.isPending("peer"))
        assertEquals(300L, debounce.request("peer", nowMs = 10))
    }
}
