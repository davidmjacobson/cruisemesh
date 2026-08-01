package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.CoreLinkScheme

/**
 * The shell's side of the link contract: the core owns which spans are links,
 * this pins that the Kotlin wrapper hands them over unchanged and that the
 * ranges can be used as `AnnotatedString` offsets without adjustment.
 */
class MessageLinksTest {
    @Test
    fun `prose without a scheme has no links`() {
        assertEquals(emptyList<MessageLink>(), messageLinks("meet at cruisemesh.app at 4"))
        assertEquals(emptyList<MessageLink>(), messageLinks(""))
    }

    @Test
    fun `a link range indexes the body directly`() {
        val body = "here you go https://cruisemesh.app/r/#CMRELAY1:abc thanks"
        val links = messageLinks(body)
        assertEquals(1, links.size)
        val link = links.single()
        assertEquals("https://cruisemesh.app/r/#CMRELAY1:abc", link.url)
        // The whole point: substring == url, so what is rendered is where the
        // tap goes. String.substring counts UTF-16 units, same as the core.
        assertEquals(link.url, body.substring(link.start, link.end))
        assertEquals(CoreLinkScheme.HTTPS, link.scheme)
    }

    @Test
    fun `emoji before a link do not shift the range`() {
        val body = "🎉 https://x.example 🎉"
        val link = messageLinks(body).single()
        assertEquals(3, link.start)
        assertEquals(20, link.end)
        assertEquals(link.url, body.substring(link.start, link.end))
    }

    @Test
    fun `several links stay in order and all index the body`() {
        val body = "https://a.example then cruisemesh://r then https://b.example"
        val links = messageLinks(body)
        assertEquals(3, links.size)
        assertEquals(links.map { it.start }.sorted(), links.map { it.start })
        for (link in links) {
            assertEquals(link.url, body.substring(link.start, link.end))
        }
        assertEquals(CoreLinkScheme.CRUISE_MESH, links[1].scheme)
    }

    @Test
    fun `a tap maps to the link that covers it, and to nothing outside`() {
        val body = "go https://x.example now"
        val links = messageLinks(body)
        val link = links.single()
        assertNull(linkAtOffset(links, link.start - 1))
        assertEquals(link, linkAtOffset(links, link.start))
        assertEquals(link, linkAtOffset(links, link.end - 1))
        // Half-open: the character after the link is prose again.
        assertNull(linkAtOffset(links, link.end))
    }

    /**
     * Both of these render as the single address `https://evil.example.apple.example`
     * -- a soft hyphen draws nothing, a one-dot leader draws a full stop --
     * while only `https://evil.example` would be underlined and opened. The
     * core refuses them outright; this pins that the refusal survives the FFI
     * boundary rather than being a Rust-only property.
     */
    @Test
    fun `a hidden boundary leaves no tappable prefix`() {
        // Soft hyphen: draws nothing at all.
        val hidden = "https://evil.example\u00AD.apple.example"
        // One dot leader: draws an ordinary full stop.
        val lookalike = "https://evil.example\u2024apple.example"
        assertEquals(emptyList<MessageLink>(), messageLinks(hidden))
        assertEquals(emptyList<MessageLink>(), messageLinks(lookalike))
    }

    @Test
    fun `the openable check refuses what the detector refuses`() {
        assertEquals(CoreLinkScheme.HTTPS, openableLinkScheme("https://x.example"))
        assertEquals(CoreLinkScheme.CRUISE_MESH, openableLinkScheme("cruisemesh://r"))
        assertNull(openableLinkScheme("http://x.example"))
        assertNull(openableLinkScheme("javascript:alert(1)"))
        assertNull(openableLinkScheme("https://x.example extra"))
        assertNull(openableLinkScheme(""))
    }
}
