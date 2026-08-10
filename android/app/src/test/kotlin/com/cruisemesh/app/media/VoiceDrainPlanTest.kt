package com.cruisemesh.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class VoiceDrainPlanTest {
    @Test
    fun `a normal release drains the buffered tail`() {
        assertEquals(
            VoiceDrainPlan.DRAIN_WINDOW_MS,
            VoiceDrainPlan.drainWindowMs(alreadyFinalized = false),
        )
    }

    @Test
    fun `a max-duration backstop finalize does not drain`() {
        // selfStopped: the moov atom is already written and stop() must not run
        // again, so there is nothing to keep the (already dead) recorder open for.
        assertEquals(0L, VoiceDrainPlan.drainWindowMs(alreadyFinalized = true))
    }

    @Test
    fun `the drain window sits in the field-measured tail-loss range`() {
        // The observed loss on two phones was ~0.4-0.5 s; the window must cover it
        // without ballooning the trailing ambient audio.
        assertTrue(
            "drain window ${VoiceDrainPlan.DRAIN_WINDOW_MS}ms outside 400-600ms",
            VoiceDrainPlan.DRAIN_WINDOW_MS in 400L..600L,
        )
    }

    @Test
    fun `min-buffer latency is an order of magnitude below the drain window`() {
        // A typical AudioRecord min buffer is a few KB; at the plan's sample rate
        // that is tens of ms, which is why the fixed window (not a min-buffer
        // derivation) is what ships.
        val sampleRateHz = 16_000
        val minBufferBytes = 4_096 // ~2048 frames of 16-bit mono
        val latency = VoiceDrainPlan.minBufferLatencyMs(minBufferBytes, sampleRateHz)
        assertEquals(2_048L * 1000L / 16_000L, latency)
        assertTrue(
            "min-buffer latency ${latency}ms should be well under the drain window",
            latency < VoiceDrainPlan.DRAIN_WINDOW_MS / 2,
        )
    }

    @Test
    fun `min-buffer latency guards degenerate inputs`() {
        assertEquals(0L, VoiceDrainPlan.minBufferLatencyMs(minBufferBytes = 4_096, sampleRateHz = 0))
        assertEquals(0L, VoiceDrainPlan.minBufferLatencyMs(minBufferBytes = -1, sampleRateHz = 16_000))
    }
}
