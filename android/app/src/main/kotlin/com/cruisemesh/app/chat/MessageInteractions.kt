package com.cruisemesh.app.chat

import com.cruisemesh.app.media.KIND_REACTION
import uniffi.cruisemesh_core.CoreMessageTarget
import uniffi.cruisemesh_core.CoreReactionPayload
import uniffi.cruisemesh_core.decodeReactionPayload
import uniffi.cruisemesh_core.encodeReactionPayload
import uniffi.cruisemesh_core.coreReactionSummariesByTarget
import uniffi.cruisemesh_core.coreVisibleGapIndices
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
    fun encode(): ByteArray = encodeReactionPayload(
        CoreReactionPayload(
            target = CoreMessageTarget(target.senderUserId, target.lamport, target.kind),
            emoji = emoji,
        ),
    )

    companion object {
        fun decode(bytes: ByteArray): ReactionPayload? {
            val decoded = decodeReactionPayload(bytes) ?: return null
            return ReactionPayload(
                target = MessageTarget(decoded.target.senderUserId, decoded.target.lamport, decoded.target.kind),
                emoji = decoded.emoji,
            )
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
    return coreReactionSummariesByTarget(messages, ownUserId).associate { target ->
        val key = MessageTarget(target.target.senderUserId, target.target.lamport, target.target.kind).stableKey
        key to target.reactions.map { ReactionSummary(it.emoji, it.count.toInt(), it.reactedByOwnUser) }
    }
}

fun visibleGapIndices(messages: List<StoredMessage>, visibleMessages: List<StoredMessage>): Set<Int> =
    coreVisibleGapIndices(messages).mapTo(mutableSetOf()) { it.toInt() }

private fun hex(bytes: ByteArray): String =
    bytes.joinToString(separator = "") { "%02x".format(it) }
