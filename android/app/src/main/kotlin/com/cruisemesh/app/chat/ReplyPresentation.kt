package com.cruisemesh.app.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage

private const val ORIGINAL_UNAVAILABLE = "Original message unavailable"

/** UI-ready reply metadata for one timeline message. */
data class MessageReplyMetadata(
    val msgId: ByteArray?,
    val quoted: QuotedMessagePreview?,
)

/** Compact presentation of the message referenced by a reply. */
data class QuotedMessagePreview(
    val senderLabel: String?,
    val text: String,
    val target: StoredMessage?,
)

/** Stable key shared with reactions and lazy-list items. */
fun messageStableKey(message: StoredMessage): String =
    MessageTarget(message.senderUserId, message.lamport, message.kind).stableKey

/**
 * Per-message store lookups the chat screens need alongside the message list
 * itself (FA4): reply-quote previews and, for the sender's own messages, the
 * outbound-expiry watermark used to render "Not delivered". Both used to be
 * queried directly from `LazyColumn` item lambdas / composition-time
 * `remember` blocks -- see [loadChatExtras] for the off-main-thread load that
 * replaces those call sites.
 */
data class ChatExtras(
    val replyMetadata: Map<String, MessageReplyMetadata> = emptyMap(),
    val outboundExpiryMs: Map<String, Long?> = emptyMap(),
)

/**
 * Loads [ChatExtras] for [messages] in one store pass. Callers run this off
 * the main thread (`produceState` + `Dispatchers.IO`) whenever the message
 * list changes, and look up results from the returned maps during
 * composition/recomposition instead of calling [store] there directly.
 */
fun loadChatExtras(
    store: MessageStore,
    messages: List<StoredMessage>,
    ownUserId: ByteArray,
    senderLabelFor: (StoredMessage) -> String,
): ChatExtras {
    val replyMetadata = loadMessageReplyMetadata(store, messages, senderLabelFor)
    val outboundExpiryMs = messages
        .filter { it.senderUserId.contentEquals(ownUserId) }
        .associate { messageStableKey(it) to store.outboundMessageExpiry(it.chatId, it.senderUserId, it.lamport) }
    return ChatExtras(replyMetadata, outboundExpiryMs)
}

/**
 * Loads stable envelope IDs and resolves reply targets once per message-list
 * refresh, rather than reopening ciphertext or querying the store during each
 * bubble recomposition.
 */
fun loadMessageReplyMetadata(
    store: MessageStore,
    messages: List<StoredMessage>,
    senderLabelFor: (StoredMessage) -> String,
): Map<String, MessageReplyMetadata> = buildMap {
    for (metadata in store.replyMetadata(messages)) {
        val message = messages.first { it.senderUserId.contentEquals(metadata.message.senderUserId) &&
            it.lamport == metadata.message.lamport && it.kind == metadata.message.kind }
        val quoted = metadata.replyToMsgId?.let {
            quotedMessagePreview(
                target = metadata.target,
                senderLabelFor = senderLabelFor,
            )
        }
        put(
            messageStableKey(message),
            MessageReplyMetadata(
                msgId = metadata.msgId,
                quoted = quoted,
            ),
        )
    }
}

fun quotedMessagePreview(
    target: StoredMessage?,
    senderLabelFor: (StoredMessage) -> String,
): QuotedMessagePreview = if (target == null) {
    QuotedMessagePreview(
        senderLabel = null,
        text = ORIGINAL_UNAVAILABLE,
        target = null,
    )
} else {
    QuotedMessagePreview(
        senderLabel = senderLabelFor(target),
        text = quotedMessageText(target),
        target = target,
    )
}

fun quotedMessageText(message: StoredMessage): String = when (message.kind) {
    KIND_ATTACHMENT_MANIFEST -> AttachmentPayload.previewLabel(AttachmentPayload.decode(message.payload))
    else -> message.payload.toString(Charsets.UTF_8).trim().ifEmpty { "Message" }
}

/** Quoted block embedded at the top of a reply bubble. */
@Composable
fun QuotedMessageBlock(
    preview: QuotedMessagePreview,
    accentColor: Color,
    contentColor: Color,
    onClick: (() -> Unit)?,
    modifier: Modifier = Modifier,
) {
    val clickModifier = if (onClick == null) Modifier else Modifier.clickable(onClick = onClick)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(contentColor.copy(alpha = 0.10f))
            .then(clickModifier)
            .padding(end = 10.dp),
    ) {
        Box(
            modifier = Modifier
                .width(4.dp)
                .height(44.dp)
                .background(accentColor),
        )
        Column(
            modifier = Modifier
                .weight(1f)
                .padding(start = 8.dp, top = 6.dp, bottom = 6.dp),
        ) {
            if (preview.senderLabel != null) {
                Text(
                    text = preview.senderLabel,
                    style = MaterialTheme.typography.labelMedium,
                    color = accentColor,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Text(
                text = preview.text,
                style = MaterialTheme.typography.bodySmall,
                color = contentColor.copy(alpha = 0.86f),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** Selected reply target shown immediately above the composer. */
@Composable
fun ReplyComposerPreview(
    preview: QuotedMessagePreview,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = modifier.fillMaxWidth(),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            QuotedMessageBlock(
                preview = preview,
                accentColor = MaterialTheme.colorScheme.primary,
                contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                onClick = null,
                modifier = Modifier
                    .weight(1f)
                    .padding(6.dp),
            )
            IconButton(
                onClick = onCancel,
                // FA10: keep the 40dp visual size, restore a 48dp touch target.
                modifier = Modifier.minimumInteractiveComponentSize().size(40.dp),
            ) {
                Icon(Icons.Default.Close, contentDescription = "Cancel reply")
            }
        }
    }
}
