package com.cruisemesh.app.media

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.nio.charset.StandardCharsets

/**
 * Wire `content` for a `kind=16` attachment-manifest chat message
 * (DESIGN.md §7.1 reserved kinds, §8 media readiness).
 *
 * v1 ships the blob **inline** in the sealed chat envelope so photos and
 * short voice memos ride the existing digest/relay/outbound path without a
 * separate chunk transfer. The layout is versioned so a later external-chunk
 * mode (kind=17) can land without breaking older clients: unknown versions
 * decode as failure and the UI shows a placeholder.
 *
 * Layout (big-endian):
 * ```
 * version      u8   (=1)
 * media_type   u8   (1=image, 2=audio)
 * mime_len     u16 + mime UTF-8
 * duration_ms  u32  (0 for still images)
 * blob_len     u32 + blob
 * caption_len  u16 + caption UTF-8 (may be empty)
 * ```
 */
data class AttachmentPayload(
    val mediaType: MediaType,
    val mimeType: String,
    val durationMs: Int,
    val blob: ByteArray,
    val caption: String = "",
) {
    enum class MediaType(val wire: Int) {
        IMAGE(1),
        AUDIO(2),
        ;

        companion object {
            fun fromWire(value: Int): MediaType? = entries.firstOrNull { it.wire == value }
        }
    }

    fun encode(): ByteArray {
        val out = ByteArrayOutputStream()
        DataOutputStream(out).use { data ->
            data.writeByte(WIRE_VERSION)
            data.writeByte(mediaType.wire)
            writeUtf16(data, mimeType)
            data.writeInt(durationMs.coerceAtLeast(0))
            data.writeInt(blob.size)
            data.write(blob)
            writeUtf16(data, caption)
        }
        return out.toByteArray()
    }

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is AttachmentPayload) return false
        return mediaType == other.mediaType &&
            mimeType == other.mimeType &&
            durationMs == other.durationMs &&
            blob.contentEquals(other.blob) &&
            caption == other.caption
    }

    override fun hashCode(): Int {
        var result = mediaType.hashCode()
        result = 31 * result + mimeType.hashCode()
        result = 31 * result + durationMs
        result = 31 * result + blob.contentHashCode()
        result = 31 * result + caption.hashCode()
        return result
    }

    companion object {
        const val WIRE_VERSION: Int = 1

        /** Soft cap for inline blobs (BLE-friendly; DESIGN.md §5.2 / §8). */
        const val MAX_BLOB_BYTES: Int = 180 * 1024

        fun decode(bytes: ByteArray): AttachmentPayload? {
            if (bytes.isEmpty()) return null
            return try {
                DataInputStream(ByteArrayInputStream(bytes)).use { data ->
                    val version = data.readUnsignedByte()
                    if (version != WIRE_VERSION) return null
                    val mediaType = MediaType.fromWire(data.readUnsignedByte()) ?: return null
                    val mime = readUtf16(data) ?: return null
                    val durationMs = data.readInt()
                    if (durationMs < 0) return null
                    val blobLen = data.readInt()
                    if (blobLen < 0 || blobLen > MAX_BLOB_BYTES * 2) return null
                    val blob = ByteArray(blobLen)
                    data.readFully(blob)
                    val caption = readUtf16(data) ?: return null
                    // Trailing bytes are ignored so minor future extensions stay forward-compatible.
                    AttachmentPayload(
                        mediaType = mediaType,
                        mimeType = mime,
                        durationMs = durationMs,
                        blob = blob,
                        caption = caption,
                    )
                }
            } catch (_: Exception) {
                null
            }
        }

        fun previewLabel(payload: AttachmentPayload?): String = when (payload?.mediaType) {
            MediaType.IMAGE -> if (payload.caption.isNotBlank()) "📷 ${payload.caption}" else "📷 Photo"
            MediaType.AUDIO -> {
                val secs = ((payload.durationMs + 500) / 1000).coerceAtLeast(1)
                if (payload.caption.isNotBlank()) "🎤 ${payload.caption}" else "🎤 Voice memo ($secs s)"
            }
            null -> "Attachment"
        }

        fun previewLabelFromRaw(kind: UByte, payload: ByteArray): String? {
            if (kind != KIND_ATTACHMENT_MANIFEST) return null
            return previewLabel(decode(payload))
        }

        private fun writeUtf16(data: DataOutputStream, value: String) {
            val encoded = value.toByteArray(StandardCharsets.UTF_8)
            require(encoded.size <= 0xFFFF) { "string too long" }
            data.writeShort(encoded.size)
            data.write(encoded)
        }

        private fun readUtf16(data: DataInputStream): String? {
            val len = data.readUnsignedShort()
            val buf = ByteArray(len)
            data.readFully(buf)
            return String(buf, StandardCharsets.UTF_8)
        }
    }
}

/** DESIGN.md §7.1 reserved: attachment-manifest (chat-stream message). */
const val KIND_ATTACHMENT_MANIFEST: UByte = 16u

/** DESIGN.md §7.1 reserved: attachment-chunk (not used for inline v1). */
const val KIND_ATTACHMENT_CHUNK: UByte = 17u

/** DESIGN.md §7.1 extension: a hidden chat-stream reaction targeting another message. */
const val KIND_REACTION: UByte = 18u

/** DESIGN.md §7.1: pairwise-sealed group invite (shown as a system line). */
const val KIND_GROUP_INVITE: UByte = 4u

/** Kinds rendered in the conversation timeline. */
fun isVisibleChatKind(kind: UByte): Boolean =
    kind == 1u.toUByte() || kind == KIND_ATTACHMENT_MANIFEST || kind == KIND_GROUP_INVITE
