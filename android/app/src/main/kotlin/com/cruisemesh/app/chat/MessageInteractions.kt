package com.cruisemesh.app.chat

import com.cruisemesh.app.media.KIND_REACTION
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.nio.charset.StandardCharsets
import uniffi.cruisemesh_core.StoredMessage

class MessageTarget(
    val senderUserId: ByteArray,
    val lamport: ULong,
    val kind: UByte,
) {
    val stableKey: String = "${hex(senderUserId)}:${lamport}:${kind}"

    override fun equals(other: Any?): Boolean =
        other is MessageTarget &&
            senderUserId.contentEquals(other.senderUserId) &&
            lamport == other.lamport &&
            kind == other.kind

    override fun hashCode(): Int {
        var result = senderUserId.contentHashCode()
        result = 31 * result + lamport.hashCode()
        result = 31 * result + kind.hashCode()
        return result
    }
}

data class ReactionPayload(
    val target: MessageTarget,
    val emoji: String,
) {
    fun encode(): ByteArray {
        val out = ByteArrayOutputStream()
        DataOutputStream(out).use { data ->
            data.writeByte(WIRE_VERSION)
            writeBytes16(data, target.senderUserId)
            data.writeLong(target.lamport.toLong())
            data.writeByte(target.kind.toInt())
            writeUtf16(data, emoji)
        }
        return out.toByteArray()
    }

    companion object {
        private const val WIRE_VERSION = 1
        private const val MAX_EMOJI_BYTES = 32

        fun decode(bytes: ByteArray): ReactionPayload? {
            if (bytes.isEmpty()) return null
            return try {
                DataInputStream(ByteArrayInputStream(bytes)).use { data ->
                    val version = data.readUnsignedByte()
                    if (version != WIRE_VERSION) return null
                    val senderUserId = readBytes16(data) ?: return null
                    val lamport = data.readLong().toULong()
                    val kind = data.readUnsignedByte().toUByte()
                    val emoji = readUtf16(data, MAX_EMOJI_BYTES) ?: return null
                    if (data.available() != 0) return null
                    if (emoji.isNotEmpty() && emoji.toByteArray(StandardCharsets.UTF_8).size > MAX_EMOJI_BYTES) {
                        return null
                    }
                    ReactionPayload(
                        target = MessageTarget(senderUserId, lamport, kind),
                        emoji = emoji,
                    )
                }
            } catch (_: Exception) {
                null
            }
        }
    }
}

data class ReactionSummary(
    val emoji: String,
    val count: Int,
    val reactedByOwnUser: Boolean,
)

fun reactionSummariesByTarget(
    messages: List<StoredMessage>,
    ownUserId: ByteArray,
): Map<String, List<ReactionSummary>> {
    val reactionsByTarget = linkedMapOf<String, LinkedHashMap<String, ReactionState>>()
    for (message in messages) {
        if (message.kind != KIND_REACTION) continue
        val reaction = ReactionPayload.decode(message.payload) ?: continue
        val targetReactions = reactionsByTarget.getOrPut(reaction.target.stableKey) { linkedMapOf() }
        val reactorKey = hex(message.senderUserId)
        val previous = targetReactions[reactorKey]
        if (previous == null || message.lamport >= previous.lamport) {
            if (reaction.emoji.isBlank()) {
                targetReactions.remove(reactorKey)
            } else {
                targetReactions[reactorKey] = ReactionState(
                    lamport = message.lamport,
                    emoji = reaction.emoji,
                    reactedByOwnUser = message.senderUserId.contentEquals(ownUserId),
                )
            }
        }
    }
    return reactionsByTarget.mapValues { (_, byReactor) ->
        byReactor.values
            .groupBy { it.emoji }
            .map { (emoji, entries) ->
                ReactionSummary(
                    emoji = emoji,
                    count = entries.size,
                    reactedByOwnUser = entries.any { it.reactedByOwnUser },
                )
            }
            .sortedWith(compareByDescending<ReactionSummary> { it.reactedByOwnUser }.thenBy { it.emoji })
    }
}

private data class ReactionState(
    val lamport: ULong,
    val emoji: String,
    val reactedByOwnUser: Boolean,
)

fun visibleGapIndices(messages: List<StoredMessage>, visibleMessages: List<StoredMessage>): Set<Int> {
    val visibleKeys = visibleMessages.mapIndexed { index, msg ->
        MessageTarget(msg.senderUserId, msg.lamport, msg.kind).stableKey to index
    }.toMap()
    val result = mutableSetOf<Int>()
    val lastLamport = mutableMapOf<String, ULong>()
    for (message in messages) {
        val senderKey = hex(message.senderUserId)
        val previous = lastLamport[senderKey]
        val visibleIndex = visibleKeys[MessageTarget(message.senderUserId, message.lamport, message.kind).stableKey]
        if (visibleIndex != null && previous != null && message.lamport > previous + 1uL) {
            result.add(visibleIndex)
        }
        lastLamport[senderKey] = maxOf(previous ?: 0uL, message.lamport)
    }
    return result
}

private fun writeBytes16(data: DataOutputStream, bytes: ByteArray) {
    require(bytes.size <= 0xFFFF) { "byte field too long" }
    data.writeShort(bytes.size)
    data.write(bytes)
}

private fun readBytes16(data: DataInputStream): ByteArray? {
    val len = data.readUnsignedShort()
    val bytes = ByteArray(len)
    data.readFully(bytes)
    return bytes
}

private fun writeUtf16(data: DataOutputStream, value: String) {
    val encoded = value.toByteArray(StandardCharsets.UTF_8)
    require(encoded.size <= 0xFFFF) { "string too long" }
    data.writeShort(encoded.size)
    data.write(encoded)
}

private fun readUtf16(data: DataInputStream, maxBytes: Int): String? {
    val len = data.readUnsignedShort()
    if (len > maxBytes) return null
    val buf = ByteArray(len)
    data.readFully(buf)
    return String(buf, StandardCharsets.UTF_8)
}

private fun hex(bytes: ByteArray): String =
    bytes.joinToString(separator = "") { "%02x".format(it) }
