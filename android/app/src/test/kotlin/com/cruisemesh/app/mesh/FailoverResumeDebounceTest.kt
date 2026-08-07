package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
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
        val arm = requireNotNull(debounce.request(peer, nowMs = 1_000))
        assertEquals(300L, arm.delayMs)
        assertNull(debounce.request(peer, nowMs = 1_090))
        assertNull(debounce.request(peer, nowMs = 1_240))
        assertTrue(debounce.isPending(peer))

        debounce.fired(peer, arm.token)
        assertFalse(debounce.isPending(peer))
        // A genuinely later failover is a new burst.
        assertNotNull(debounce.request(peer, nowMs = 5_000))
    }

    @Test
    fun `a stale timer cannot clear a newly armed window`() {
        val debounce = FailoverResumeDebounce(windowMs = 300)
        val first = requireNotNull(debounce.request("peer", nowMs = 0))
        // The window elapses and a fresh disconnect re-arms it before the first
        // timer's message is dispatched.
        val second = requireNotNull(debounce.request("peer", nowMs = 300))
        assertNotEquals(first.token, second.token)

        // The first timer now runs. Clearing the *new* window's marker here is
        // what would let one burst resume twice.
        debounce.fired("peer", first.token)
        assertTrue(debounce.isPending("peer"))
        assertNull(debounce.request("peer", nowMs = 310))
    }

    @Test
    fun `two peers failing over together each get their own resume`() {
        val debounce = FailoverResumeDebounce(windowMs = 300)
        assertNotNull(debounce.request("peer-a", nowMs = 0))
        assertNotNull(debounce.request("peer-b", nowMs = 5))
    }

    @Test
    fun `clear drops pending windows so a restarted service is not wedged`() {
        val debounce = FailoverResumeDebounce(windowMs = 300)
        assertNotNull(debounce.request("peer", nowMs = 0))
        debounce.clear()
        assertFalse(debounce.isPending("peer"))
        assertNotNull(debounce.request("peer", nowMs = 10))
    }
}
