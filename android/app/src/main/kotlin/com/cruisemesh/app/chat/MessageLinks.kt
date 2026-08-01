package com.cruisemesh.app.chat

import uniffi.cruisemesh_core.CoreLinkScheme
import uniffi.cruisemesh_core.coreDetectLinks
import uniffi.cruisemesh_core.coreLinkOpenableScheme

/**
 * One link inside a message body: a half-open range over the body's UTF-16
 * code units (which is exactly what Kotlin `String`/`AnnotatedString` index
 * by) plus the destination.
 *
 * [url] is byte-for-byte `body.substring(start, end)`. Render the substring,
 * open [url] -- never a rewritten, prettified or completed version of either.
 * That equality is what makes display-text spoofing impossible rather than
 * something the shell has to police.
 */
data class MessageLink(
    val start: Int,
    val end: Int,
    val url: String,
    val scheme: CoreLinkScheme,
)

/**
 * The links in [body], in order and non-overlapping.
 *
 * Detection lives in the Rust core ([coreDetectLinks]) so the scheme
 * allow-list cannot drift between Android and iOS: no re-implementation here,
 * and deliberately not `Linkify`, which would happily linkify `http://`,
 * `mailto:` and bare domains.
 */
fun messageLinks(body: String): List<MessageLink> =
    coreDetectLinks(body).map { detected ->
        MessageLink(
            start = detected.startUtf16.toInt(),
            end = detected.endUtf16.toInt(),
            url = detected.url,
            scheme = detected.scheme,
        )
    }

/** The link covering character [offset], or null when the tap missed them all. */
fun linkAtOffset(links: List<MessageLink>, offset: Int): MessageLink? =
    links.firstOrNull { offset >= it.start && offset < it.end }

/**
 * The scheme a bare destination string may be opened with, or null to refuse.
 *
 * The shell must not add a second scheme check of its own: if something should
 * be allowed or refused, `core/src/link_detect.rs` changes and both platforms
 * move together.
 */
fun openableLinkScheme(url: String): CoreLinkScheme? = coreLinkOpenableScheme(url)
