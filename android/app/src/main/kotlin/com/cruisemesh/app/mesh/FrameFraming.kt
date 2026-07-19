package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.BleFrameReassembler
import uniffi.cruisemesh_core.bleAttHeaderOverhead
import uniffi.cruisemesh_core.bleDefaultAttMtu
import uniffi.cruisemesh_core.bleMaxAttValueLen
import uniffi.cruisemesh_core.fragmentBleFrame

/**
 * Bytes of per-fragment link-layer header: a 16-bit index and 16-bit total,
 * each big-endian. A 1-byte-each header (max 255 fragments) capped a frame at
 * ~130 KB, which a photo attachment (~170 KB) blows past -- [FrameFraming.fragment]
 * then threw "frame too large to fragment" and, because `sendFrame` (not the
 * later `sendNextQueuedFragment`) issues the split, that throw unwound the GATT
 * callback and killed the link ("photos don't load"). 16-bit fields permit
 * photo-scale frames while the shared core enforces its bounded P2P ceiling.
 *
 * This is a link-layer wire change: both peers must run code that reads/writes
 * this identical layout ([FrameReassembler] parses it). Fine for a fleet that
 * updates together.
 */
object FrameFraming {
    const val MAX_FRAGMENTS: Int = 65_535
    val ATT_HEADER_OVERHEAD: Int get() = bleAttHeaderOverhead().toInt()
    val DEFAULT_ATT_MTU: Int get() = bleDefaultAttMtu().toInt()
    val MAX_ATT_VALUE_LEN: Int get() = bleMaxAttValueLen().toInt()

    fun fragmentOrNull(frame: ByteArray, mtuPayloadSize: Int): List<ByteArray>? =
        fragmentBleFrame(frame, mtuPayloadSize.toUInt())

    fun fragment(frame: ByteArray, mtuPayloadSize: Int): List<ByteArray> =
        fragmentOrNull(frame, mtuPayloadSize)
            ?: throw IllegalArgumentException("frame too large to fragment")
}

/**
 * Reassembles one peer's ordered fragment stream back into full frames.
 * Assumes fragments of a single frame arrive in order and aren't interleaved
 * with another frame's fragments -- true on a single BLE connection, which
 * serializes writes/notifications over one link.
 */
class FrameReassembler {
    private val core = BleFrameReassembler()

    /** Feed one fragment; returns the reassembled frame once complete, else null. */
    fun accept(fragment: ByteArray): ByteArray? = core.accept(fragment)
}
