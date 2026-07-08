package com.cruisemesh.app.mesh

import kotlin.random.Random
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class FrameFramingTest {

    @Test
    fun `frame smaller than payload size produces a single fragment`() {
        val frame = "hi".toByteArray()
        val fragments = FrameFraming.fragment(frame, mtuPayloadSize = 20)
        assertEquals(1, fragments.size)
    }

    @Test
    fun `default ATT MTU splits a long frame into multiple fragments`() {
        val frame = Random.nextBytes(100)
        val payloadSize = FrameFraming.DEFAULT_ATT_MTU - FrameFraming.ATT_HEADER_OVERHEAD
        val fragments = FrameFraming.fragment(frame, payloadSize)
        assertEquals(6, fragments.size) // ceil(100 / (23 - 3 - 2))

        val reassembler = FrameReassembler()
        var result: ByteArray? = null
        for (fragment in fragments) {
            result = reassembler.accept(fragment) ?: continue
        }
        assertArrayEquals(frame, result)
    }

    @Test
    fun `reassembly returns null until the last fragment arrives`() {
        val frame = Random.nextBytes(50)
        val fragments = FrameFraming.fragment(frame, mtuPayloadSize = 10)
        check(fragments.size > 1)

        val reassembler = FrameReassembler()
        fragments.dropLast(1).forEach { fragment ->
            assertNull(reassembler.accept(fragment))
        }
        assertArrayEquals(frame, reassembler.accept(fragments.last()))
    }

    @Test
    fun `a dropped middle fragment desyncs and discards the partial frame`() {
        val frame = Random.nextBytes(50)
        val fragments = FrameFraming.fragment(frame, mtuPayloadSize = 10)
        check(fragments.size > 2)

        val reassembler = FrameReassembler()
        reassembler.accept(fragments[0])
        // Skip fragments[1]; index 2 arrives when index 1 was expected.
        assertNull(reassembler.accept(fragments[2]))
    }

    @Test
    fun `round trips exactly at a chunk-size boundary`() {
        val payloadSize = 12
        val chunkSize = payloadSize - 2
        val frame = Random.nextBytes(chunkSize * 3)
        val fragments = FrameFraming.fragment(frame, payloadSize)
        assertEquals(3, fragments.size)

        val reassembler = FrameReassembler()
        val result = fragments.dropLast(1).onEach { reassembler.accept(it) }
            .let { reassembler.accept(fragments.last()) }
        assertArrayEquals(frame, result)
    }
}
