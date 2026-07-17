package com.cruisemesh.app.media

import uniffi.cruisemesh_core.AttachmentMediaType
import uniffi.cruisemesh_core.CoreAttachmentPayload
import uniffi.cruisemesh_core.attachmentMaxBlobBytes
import uniffi.cruisemesh_core.decodeAttachmentPayload
import uniffi.cruisemesh_core.encodeAttachmentPayload

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

    fun encode(): ByteArray = encodeAttachmentPayload(
        CoreAttachmentPayload(
            mediaType = when (mediaType) {
                MediaType.IMAGE -> AttachmentMediaType.IMAGE
                MediaType.AUDIO -> AttachmentMediaType.AUDIO
            },
            mimeType = mimeType,
            durationMs = durationMs.toLong().coerceAtLeast(0),
            blob = blob,
            caption = caption,
        ),
    )

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
        /** Soft cap for inline blobs (BLE-friendly; DESIGN.md §5.2 / §8). */
        val MAX_BLOB_BYTES: Int get() = attachmentMaxBlobBytes().toInt()

        fun decode(bytes: ByteArray): AttachmentPayload? {
            val decoded = decodeAttachmentPayload(bytes) ?: return null
            return AttachmentPayload(
                mediaType = when (decoded.mediaType) {
                    AttachmentMediaType.IMAGE -> MediaType.IMAGE
                    AttachmentMediaType.AUDIO -> MediaType.AUDIO
                },
                mimeType = decoded.mimeType,
                durationMs = decoded.durationMs.toInt(),
                blob = decoded.blob,
                caption = decoded.caption,
            )
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
    uniffi.cruisemesh_core.coreIsVisibleChatKind(kind)
