package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SwipeToReplyLogicTest {
    @Test
    fun leftwardDragIsIgnored() {
        assertEquals(0f, SwipeToReplyLogic.clampOffset(-40f, 96f), 0f)
        assertEquals(0f, SwipeToReplyLogic.clampOffset(0f, 96f), 0f)
    }

    @Test
    fun withinMaxDragTracksTheFingerOneToOne() {
        assertEquals(50f, SwipeToReplyLogic.clampOffset(50f, 96f), 0f)
        assertEquals(96f, SwipeToReplyLogic.clampOffset(96f, 96f), 0f)
    }

    @Test
    fun beyondMaxDragRubberBands() {
        // 96 + (200-96)*0.15 = 96 + 15.6 = 111.6
        assertEquals(111.6f, SwipeToReplyLogic.clampOffset(200f, 96f), 0.01f)
        // Always stays past the cap but grows far slower than the finger.
        assertTrue(SwipeToReplyLogic.clampOffset(500f, 96f) > 96f)
        assertTrue(SwipeToReplyLogic.clampOffset(500f, 96f) < 500f)
    }

    @Test
    fun repliesOnlyPastThreshold() {
        assertFalse(SwipeToReplyLogic.shouldReply(40f, 64f))
        assertTrue(SwipeToReplyLogic.shouldReply(64f, 64f))
        assertTrue(SwipeToReplyLogic.shouldReply(80f, 64f))
    }

    @Test
    fun progressIsClampedZeroToOne() {
        assertEquals(0f, SwipeToReplyLogic.progress(0f, 64f), 0f)
        assertEquals(0.5f, SwipeToReplyLogic.progress(32f, 64f), 0f)
        assertEquals(1f, SwipeToReplyLogic.progress(64f, 64f), 0f)
        assertEquals(1f, SwipeToReplyLogic.progress(120f, 64f), 0f)
        assertEquals(0f, SwipeToReplyLogic.progress(10f, 0f), 0f)
    }
}
