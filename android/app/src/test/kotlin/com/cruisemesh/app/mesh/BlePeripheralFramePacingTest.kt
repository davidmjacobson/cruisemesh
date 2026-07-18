package com.cruisemesh.app.mesh

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BlePeripheralFramePacingTest {

    @Test
    fun `a fragment continuing an already-started frame is never paced`() {
        // Regardless of how deep the queue is, mid-frame fragments must go
        // out immediately -- pacing only applies to a new frame's first
        // fragment.
        assertFalse(shouldPaceFrameStart(startingNewFrame = false, queuedFrames = 100, threshold = 3))
    }

    @Test
    fun `a new frame is not paced while the backlog is shallow`() {
        assertFalse(shouldPaceFrameStart(startingNewFrame = true, queuedFrames = 2, threshold = 3))
    }

    @Test
    fun `a new frame is paced exactly at the deep-queue threshold`() {
        assertTrue(shouldPaceFrameStart(startingNewFrame = true, queuedFrames = 3, threshold = 3))
    }

    @Test
    fun `the common case of one or two frames in flight never gets paced`() {
        for (queuedFrames in 0..2) {
            assertFalse(shouldPaceFrameStart(startingNewFrame = true, queuedFrames = queuedFrames, threshold = 3))
        }
    }
}
