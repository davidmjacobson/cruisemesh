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
