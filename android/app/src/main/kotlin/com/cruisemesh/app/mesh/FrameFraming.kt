package com.cruisemesh.app.mesh

import java.io.ByteArrayOutputStream

/**
 * Link-layer frame fragmentation for BLE writes/notifications (DESIGN.md
 * §5.2): envelopes larger than the negotiated MTU are split into chunks each
 * prefixed with a 2-byte [index, total] header. Callers compute
 * `attMtu - ATT_HEADER_OVERHEAD` for [fragment]'s `payloadSize`.
 */
object FrameFraming {
    /** ATT opcode + handle overhead subtracted from a negotiated MTU to get usable payload. */
    const val ATT_HEADER_OVERHEAD = 3

    /** Usable payload at the default (un-negotiated) ATT MTU of 23 bytes. */
    const val DEFAULT_ATT_MTU = 23

    private const val HEADER_SIZE = 2
    private const val MAX_FRAGMENTS = 255

    /** Split [frame] into BLE-sized fragments, each prefixed with [index, total]. */
    fun fragment(frame: ByteArray, mtuPayloadSize: Int): List<ByteArray> {
        val chunkSize = (mtuPayloadSize - HEADER_SIZE).coerceAtLeast(1)
        val total = maxOf(1, (frame.size + chunkSize - 1) / chunkSize)
        require(total <= MAX_FRAGMENTS) {
            "frame too large to fragment: ${frame.size} bytes needs $total fragments (max $MAX_FRAGMENTS)"
        }
        return (0 until total).map { index ->
            val start = index * chunkSize
            val end = minOf(start + chunkSize, frame.size)
            ByteArray(HEADER_SIZE + (end - start)).also { out ->
                out[0] = index.toByte()
                out[1] = total.toByte()
                frame.copyInto(out, HEADER_SIZE, start, end)
            }
        }
    }
}

/**
 * Reassembles one peer's ordered fragment stream back into full frames.
 * Assumes fragments of a single frame arrive in order and aren't interleaved
 * with another frame's fragments -- true on a single BLE connection, which
 * serializes writes/notifications over one link.
 */
class FrameReassembler {
    private var buffer: ByteArrayOutputStream? = null
    private var expectedTotal = 0
    private var nextIndex = 0

    /** Feed one fragment; returns the reassembled frame once complete, else null. */
    fun accept(fragment: ByteArray): ByteArray? {
        if (fragment.size < 2) return null
        val index = fragment[0].toInt() and 0xFF
        val total = fragment[1].toInt() and 0xFF
        if (index == 0) {
            buffer = ByteArrayOutputStream()
            expectedTotal = total
            nextIndex = 0
        }
        val active = buffer
        if (active == null || index != nextIndex || total != expectedTotal) {
            // Out-of-order or desynced fragment (e.g. we missed fragment 0):
            // drop the partial frame rather than risk stitching mismatched data.
            buffer = null
            return null
        }
        active.write(fragment, 2, fragment.size - 2)
        nextIndex++
        if (nextIndex < expectedTotal) return null
        buffer = null
        return active.toByteArray()
    }
}
