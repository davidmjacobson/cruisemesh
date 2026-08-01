package com.cruisemesh.app.chat

import android.content.Intent
import android.net.Uri
import android.widget.Toast
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import com.cruisemesh.app.R
import uniffi.cruisemesh_core.CoreLinkScheme

/**
 * What a tap or a long-press on a message body should do.
 *
 * Passed in rather than read from the bubble's own `combinedClickable`
 * because a link needs the *position* of the tap, which `combinedClickable`
 * does not hand out. [MessageBodyText] therefore takes over both gestures for
 * the text itself and re-emits the bubble's behaviour: [onClick] for a tap
 * that missed every link, [onLongClick] for the reaction/copy overlay --
 * including a long-press that lands squarely on a link, which is the gesture
 * a built-in `LinkAnnotation` would have swallowed.
 *
 * Null (the [MessageFocusOverlay]'s floating copy) means links are styled but
 * inert, and the surrounding composable's gestures are untouched.
 */
@Stable
class MessageBodyActions(
    val onLinkClick: (MessageLink) -> Unit,
    val onClick: () -> Unit,
    val onLongClick: () -> Unit,
)

/**
 * A message body with its links underlined and tappable.
 *
 * The ranges come from the core ([messageLinks]); the text rendered is the
 * body itself, untouched, so what is on screen is always the destination.
 */
@Composable
fun MessageBodyText(
    body: String,
    isOwn: Boolean,
    modifier: Modifier = Modifier,
    style: TextStyle = LocalTextStyle.current,
    actions: MessageBodyActions? = null,
) {
    val links = remember(body) { messageLinks(body) }
    // Sent and received bubbles have different backgrounds, so the link colour
    // has to differ too: inversePrimary is the tone Material picks for content
    // on a primary-filled surface, primary for content on the neutral one.
    // Both are underlined, which carries the affordance even where a contact's
    // own colour tints the received bubble.
    val linkColor = if (isOwn) {
        MaterialTheme.colorScheme.inversePrimary
    } else {
        MaterialTheme.colorScheme.primary
    }
    val text = remember(body, links, linkColor) {
        if (links.isEmpty()) {
            AnnotatedString(body)
        } else {
            buildAnnotatedString {
                append(body)
                val linkStyle = SpanStyle(
                    color = linkColor,
                    fontWeight = FontWeight.Medium,
                    textDecoration = TextDecoration.Underline,
                )
                for (link in links) {
                    addStyle(linkStyle, link.start, link.end)
                }
            }
        }
    }

    var layout by remember(body) { mutableStateOf<TextLayoutResult?>(null) }
    val currentActions = rememberUpdatedState(actions)
    // Hit-testing a tap position is invisible to TalkBack, so offer each link
    // as a custom action on the bubble instead -- otherwise the links would be
    // reachable by sighted users only.
    val linkLabels = links.map { stringResource(R.string.ui_open_link_named, it.url) }
    val semantics = if (links.isEmpty() || actions == null) {
        Modifier
    } else {
        Modifier.semantics {
            customActions = links.mapIndexed { index, link ->
                CustomAccessibilityAction(linkLabels[index]) {
                    val handling = currentActions.value ?: return@CustomAccessibilityAction false
                    handling.onLinkClick(link)
                    true
                }
            }
        }
    }
    // No links, or nothing to do with them: add no pointer input at all, so a
    // plain bubble keeps exactly the gesture handling it had before.
    val gestures = if (links.isEmpty() || actions == null) {
        Modifier
    } else {
        Modifier.pointerInput(links) {
            detectTapGestures(
                onLongPress = { currentActions.value?.onLongClick?.invoke() },
                onTap = { position ->
                    val handling = currentActions.value ?: return@detectTapGestures
                    val hit = layout?.let { linkAt(it, position, links) }
                    if (hit != null) handling.onLinkClick(hit) else handling.onClick()
                },
            )
        }
    }

    Text(
        text = text,
        style = style,
        modifier = modifier.then(semantics).then(gestures),
        onTextLayout = { layout = it },
    )
}

/**
 * The link under [position], or null.
 *
 * `getOffsetForPosition` snaps to the nearest character even when the tap
 * landed in the empty space past the end of a short line, so the tap is first
 * required to be inside the line's own box -- otherwise tapping the blank
 * corner of a bubble would open whatever link ended that line.
 */
private fun linkAt(
    layout: TextLayoutResult,
    position: Offset,
    links: List<MessageLink>,
): MessageLink? {
    val line = layout.getLineForVerticalPosition(position.y)
    if (position.y < layout.getLineTop(line) || position.y > layout.getLineBottom(line)) return null
    if (position.x < layout.getLineLeft(line) || position.x > layout.getLineRight(line)) return null
    return linkAtOffset(links, layout.getOffsetForPosition(position))
}

/**
 * Where a tapped link goes. Create one per chat screen with
 * [rememberMessageLinkHandler], hand [MessageLinkHandler.open] to the bubbles,
 * and render [MessageLinkPrompt] once alongside the screen's other overlays.
 */
@Stable
class MessageLinkHandler internal constructor() {
    internal var confirming by mutableStateOf<MessageLink?>(null)
    internal var opening by mutableStateOf<MessageLink?>(null)

    /**
     * `https://` asks first -- 6.6 says confirm before leaving the app.
     * `cruisemesh://` stays inside the app, so there is nothing to warn about.
     * Anything the core refuses is dropped silently: it was never rendered as
     * a link, so a tap here can only be a stale range, not a decision to make.
     */
    fun open(link: MessageLink) {
        when (openableLinkScheme(link.url)) {
            CoreLinkScheme.HTTPS -> confirming = link
            CoreLinkScheme.CRUISE_MESH -> opening = link
            null -> Unit
        }
    }
}

@Composable
fun rememberMessageLinkHandler(): MessageLinkHandler = remember { MessageLinkHandler() }

/** The "open this link?" confirmation, plus the in-app hand-off for `cruisemesh://`. */
@Composable
fun MessageLinkPrompt(handler: MessageLinkHandler) {
    val context = LocalContext.current
    val uriHandler = LocalUriHandler.current

    val opening = handler.opening
    LaunchedEffect(opening) {
        if (opening == null) return@LaunchedEffect
        handler.opening = null
        // setPackage keeps an in-app destination in this app: the scheme is
        // ours, but nothing stops another app from claiming it too, and a
        // chooser for our own deep link would be both confusing and a way to
        // hand a friend card to a stranger's app.
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(opening.url))
            .setPackage(context.packageName)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        runCatching { context.startActivity(intent) }.onFailure {
            Toast.makeText(context, context.getString(R.string.ui_that_link_could_not_be_opened), Toast.LENGTH_SHORT).show()
        }
    }

    val confirming = handler.confirming ?: return
    AlertDialog(
        onDismissRequest = { handler.confirming = null },
        title = { Text(stringResource(R.string.ui_open_this_link)) },
        text = { Text(stringResource(R.string.ui_this_link_opens_outside_the_chat, confirming.url)) },
        confirmButton = {
            TextButton(
                onClick = {
                    handler.confirming = null
                    runCatching { uriHandler.openUri(confirming.url) }.onFailure {
                        Toast.makeText(context, context.getString(R.string.ui_that_link_could_not_be_opened), Toast.LENGTH_SHORT).show()
                    }
                },
            ) {
                Text(stringResource(R.string.ui_open))
            }
        },
        dismissButton = {
            TextButton(onClick = { handler.confirming = null }) {
                Text(stringResource(R.string.ui_cancel))
            }
        },
    )
}
