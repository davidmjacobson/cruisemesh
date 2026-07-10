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
        assertEquals(7, fragments.size) // ceil(100 / (23 - 3 - 4-byte header))

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
    fun `fragments never exceed the GATT attribute value cap at MTU 517`() {
        // MTU 517 -> mtuPayloadSize 514, two bytes over the 512-byte ATT value
        // cap. Every fragment (header included) must still fit in 512 bytes or
        // notifyCharacteristicChanged/writeCharacteristic throw and stall the link.
        val payloadSize = 517 - FrameFraming.ATT_HEADER_OVERHEAD
        val frame = Random.nextBytes(4000)
        val fragments = FrameFraming.fragment(frame, payloadSize)

        for (fragment in fragments) {
            check(fragment.size <= FrameFraming.MAX_ATT_VALUE_LEN) {
                "fragment of ${fragment.size} bytes exceeds ${FrameFraming.MAX_ATT_VALUE_LEN}"
            }
        }

        val reassembler = FrameReassembler()
        var result: ByteArray? = null
        for (fragment in fragments) {
            result = reassembler.accept(fragment) ?: continue
        }
        assertArrayEquals(frame, result)
    }

    @Test
    fun `round trips exactly at a chunk-size boundary`() {
        val payloadSize = 12
        val chunkSize = payloadSize - 4 // 4-byte [index16, total16] header
        val frame = Random.nextBytes(chunkSize * 3)
        val fragments = FrameFraming.fragment(frame, payloadSize)
        assertEquals(3, fragments.size)

        val reassembler = FrameReassembler()
        val result = fragments.dropLast(1).onEach { reassembler.accept(it) }
            .let { reassembler.accept(fragments.last()) }
        assertArrayEquals(frame, result)
    }

    @Test
    fun `a photo-scale frame past the old 255-fragment cap round-trips`() {
        // ~170 KB, like the attachment that used to throw "frame too large to
        // fragment" (341 > 255) and kill the link. At MTU 517 this needs far
        // more than 255 fragments, which the 16-bit total field now allows.
        val payloadSize = 517 - FrameFraming.ATT_HEADER_OVERHEAD
        val frame = Random.nextBytes(170_000)
        val fragments = FrameFraming.fragment(frame, payloadSize)
        check(fragments.size > 255) { "expected >255 fragments, got ${fragments.size}" }
        for (fragment in fragments) {
            check(fragment.size <= FrameFraming.MAX_ATT_VALUE_LEN)
        }

        val reassembler = FrameReassembler()
        var result: ByteArray? = null
        for (fragment in fragments) {
            result = reassembler.accept(fragment) ?: continue
        }
        assertArrayEquals(frame, result)
    }

    @Test
    fun `fragmentOrNull returns null instead of throwing on an unfragmentable frame`() {
        // payloadSize 5 -> 1-byte chunks, so 65536 bytes needs 65536 fragments,
        // one past the 16-bit ceiling. fragment() throws; fragmentOrNull() must
        // swallow it so a GATT callback can't be unwound and kill the link.
        val frame = ByteArray(FrameFraming.MAX_FRAGMENTS + 1)
        assertNull(FrameFraming.fragmentOrNull(frame, mtuPayloadSize = 5))
    }
}
