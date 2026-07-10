package com.cruisemesh.app.mesh

import java.io.ByteArrayOutputStream

/**
 * Bytes of per-fragment link-layer header: a 16-bit index and 16-bit total,
 * each big-endian. A 1-byte-each header (max 255 fragments) capped a frame at
 * ~130 KB, which a photo attachment (~170 KB) blows past -- [FrameFraming.fragment]
 * then threw "frame too large to fragment" and, because `sendFrame` (not the
 * later `sendNextQueuedFragment`) issues the split, that throw unwound the GATT
 * callback and killed the link ("photos don't load"). 16-bit fields lift the
 * ceiling to 65535 fragments (~33 MB).
 *
 * This is a link-layer wire change: both peers must run code that reads/writes
 * this identical layout ([FrameReassembler] parses it). Fine for a fleet that
 * updates together.
 */
private const val FRAGMENT_HEADER_SIZE = 4

/**
 * Link-layer frame fragmentation for BLE writes/notifications (DESIGN.md
 * §5.2): envelopes larger than the negotiated MTU are split into chunks each
 * prefixed with a [FRAGMENT_HEADER_SIZE]-byte [index16, total16] header.
 * Callers compute `attMtu - ATT_HEADER_OVERHEAD` for [fragment]'s `payloadSize`.
 */
object FrameFraming {
    /** ATT opcode + handle overhead subtracted from a negotiated MTU to get usable payload. */
    const val ATT_HEADER_OVERHEAD = 3

    /** Usable payload at the default (un-negotiated) ATT MTU of 23 bytes. */
    const val DEFAULT_ATT_MTU = 23

    /**
     * The GATT spec caps a single attribute value (and therefore one
     * write/notification payload) at 512 bytes, independent of the negotiated
     * MTU. Modern phones negotiate MTU 517, so `mtu - ATT_HEADER_OVERHEAD` is
     * 514 -- two over this limit -- and `notifyCharacteristicChanged` /
     * `writeCharacteristic` throw `IllegalArgumentException: Notification
     * should not be longer than max length of an attribute value` on any
     * fragment that fills the chunk. That uncaught throw also unwinds the GATT
     * callback it fires in, stalling the link. So every emitted fragment must
     * fit here regardless of how large the MTU is.
     */
    const val MAX_ATT_VALUE_LEN = 512

    /** Ceiling implied by the 16-bit [FRAGMENT_HEADER_SIZE] total field. */
    const val MAX_FRAGMENTS = 65535

    /**
     * Like [fragment] but returns null instead of throwing when [frame] is too
     * large to split within [MAX_FRAGMENTS]. Callers that run on a GATT callback
     * thread use this so an over-large frame is dropped with a log rather than
     * propagated as an exception that would unwind the callback and kill the
     * link (the "photos don't load" failure mode).
     */
    fun fragmentOrNull(frame: ByteArray, mtuPayloadSize: Int): List<ByteArray>? =
        try {
            fragment(frame, mtuPayloadSize)
        } catch (_: IllegalArgumentException) {
            null
        }

    /** Split [frame] into BLE-sized fragments, each prefixed with [index16, total16]. */
    fun fragment(frame: ByteArray, mtuPayloadSize: Int): List<ByteArray> {
        // A fragment is FRAGMENT_HEADER_SIZE + chunkSize bytes, so cap the whole
        // fragment at MAX_ATT_VALUE_LEN, never just the MTU-derived size.
        val cappedPayload = mtuPayloadSize.coerceAtMost(MAX_ATT_VALUE_LEN)
        val chunkSize = (cappedPayload - FRAGMENT_HEADER_SIZE).coerceAtLeast(1)
        val total = maxOf(1, (frame.size + chunkSize - 1) / chunkSize)
        require(total <= MAX_FRAGMENTS) {
            "frame too large to fragment: ${frame.size} bytes needs $total fragments (max $MAX_FRAGMENTS)"
        }
        return (0 until total).map { index ->
            val start = index * chunkSize
            val end = minOf(start + chunkSize, frame.size)
            ByteArray(FRAGMENT_HEADER_SIZE + (end - start)).also { out ->
                out[0] = (index ushr 8).toByte()
                out[1] = index.toByte()
                out[2] = (total ushr 8).toByte()
                out[3] = total.toByte()
                frame.copyInto(out, FRAGMENT_HEADER_SIZE, start, end)
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
        if (fragment.size < FRAGMENT_HEADER_SIZE) return null
        val index = (fragment[0].toInt() and 0xFF shl 8) or (fragment[1].toInt() and 0xFF)
        val total = (fragment[2].toInt() and 0xFF shl 8) or (fragment[3].toInt() and 0xFF)
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
        active.write(fragment, FRAGMENT_HEADER_SIZE, fragment.size - FRAGMENT_HEADER_SIZE)
        nextIndex++
        if (nextIndex < expectedTotal) return null
        buffer = null
        return active.toByteArray()
    }
}
