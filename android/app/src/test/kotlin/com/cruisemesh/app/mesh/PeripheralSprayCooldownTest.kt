package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The brake on the HELLO-triggered carry-drain + digest burst after a
 * notify-reject teardown. The clock is injected (monotonic ms, like
 * [FailoverResumeDebounce] and [BleAdvertiserStateMachine]) so the window can
 * be walked exactly instead of slept through.
 */
class PeripheralSprayCooldownTest {

    @Test
    fun `the default window matches the far side's own first reconnect backoff`() {
        // The two brakes describe the same "let this settle" interval: ours
        // holds the burst back, the peer's central role holds its first retry
        // back. A cooldown longer than that would keep deferring bursts for a
        // peer that has already reconnected and settled.
        assertEquals(
            ReconnectBackoffTracker.INITIAL_BACKOFF_MS,
            PeripheralSprayCooldown.DEFAULT_WINDOW_MS,
        )
        // And far shorter than the 3-5 min re-digest interval
        // (transport_policy.rs REDIGEST_MIN_INTERVAL_MS). That gap is the whole
        // reason a gated digest is replayed rather than left to the peer's own
        // maintenance pass: the window is seconds, that fallback is minutes.
        assertTrue(PeripheralSprayCooldown.DEFAULT_WINDOW_MS < 3 * 60_000L)
    }

    @Test
    fun `an immediate reconnect is deferred, not refused`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 1_000)

        // The reconnect itself is never consulted here -- the only question
        // this class answers is how long the burst waits. A positive answer is
        // a deferral, never a refusal: nothing in this class can drop a burst.
        assertEquals(5_000L, cooldown.deferralMs("aa:bb", nowMs = 1_000))
        assertEquals(4_900L, cooldown.deferralMs("aa:bb", nowMs = 1_100))
        assertEquals(1L, cooldown.deferralMs("aa:bb", nowMs = 5_999))
    }

    @Test
    fun `every outbound frame of the exchange is gated, not just the first`() {
        // The reconnect exchange has two outbound halves on this notify path:
        // the response to the peer's HELLO, and -- once the peer answers our own
        // HELLO with its DIGEST ~200ms later -- the response to that, which is
        // the larger of the two. Gating one and not the other brakes nothing, so
        // reading the window must not consume it.
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 1_000)

        assertEquals(4_000L, cooldown.deferralMs("aa:bb", nowMs = 2_000))
        assertEquals(3_800L, cooldown.deferralMs("aa:bb", nowMs = 2_200))
        assertEquals(3_800L, cooldown.deferralMs("aa:bb", nowMs = 2_200))
        // ...and the window still ends where it was armed to end.
        assertEquals(0L, cooldown.deferralMs("aa:bb", nowMs = 6_000))
    }

    @Test
    fun `the window lapses exactly at its end and stays lapsed`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 1_000)

        assertEquals(0L, cooldown.deferralMs("aa:bb", nowMs = 6_000))
        assertEquals(0L, cooldown.deferralMs("aa:bb", nowMs = 6_001))
        assertEquals(0L, cooldown.deferralMs("aa:bb", nowMs = 900_000))
    }

    @Test
    fun `an address with no teardown history is never held back`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        cooldown.armAfterRejectTeardown("flapping", nowMs = 1_000)

        // Per-address, not a global brake: one bad link must not stall the
        // sync burst of every other central connecting in the same second.
        assertEquals(0L, cooldown.deferralMs("healthy", nowMs = 1_000))
        assertEquals(5_000L, cooldown.deferralMs("flapping", nowMs = 1_000))
    }

    @Test
    fun `a second reject teardown restarts the window rather than extending the old one`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 1_000)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 4_000)

        assertEquals(5_000L, cooldown.deferralMs("aa:bb", nowMs = 4_000))
        assertEquals(0L, cooldown.deferralMs("aa:bb", nowMs = 9_000))
    }

    @Test
    fun `a lapsed address is forgotten so a dense fleet cannot grow the map`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        // BLE addresses rotate, so in a busy room these are almost all
        // one-shot keys; remembering them past their window would be an
        // unbounded leak for the life of the process.
        repeat(50) { index -> cooldown.armAfterRejectTeardown("addr-$index", nowMs = 1_000) }
        assertEquals(50, cooldown.trackedAddressCount())

        // Reading a lapsed entry drops it...
        assertEquals(0L, cooldown.deferralMs("addr-0", nowMs = 20_000))
        assertEquals(49, cooldown.trackedAddressCount())
        // ...and so does arming a new one, which is what keeps addresses
        // nobody ever asks about again from accumulating.
        cooldown.armAfterRejectTeardown("fresh", nowMs = 20_000)
        assertEquals(1, cooldown.trackedAddressCount())
        assertEquals(5_000L, cooldown.deferralMs("fresh", nowMs = 20_000))
    }

    @Test
    fun `clearAll drops every window so a restarted peripheral role starts clean`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 1_000)

        cooldown.clearAll()

        assertEquals(0L, cooldown.deferralMs("aa:bb", nowMs = 1_000))
        assertEquals(0, cooldown.trackedAddressCount())
    }

    @Test(expected = IllegalArgumentException::class)
    fun `a zero window is rejected rather than silently disabling the brake`() {
        PeripheralSprayCooldown(windowMs = 0)
    }
}
