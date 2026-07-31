package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.ComposerReach
import uniffi.cruisemesh_core.ContactDelivery
import uniffi.cruisemesh_core.composerReach

/**
 * The composer notice is the whole fix for "someone with no pass can send but
 * never receive, and nothing tells them", so both halves are pinned here: the
 * core policy call the way the chat screen makes it, and the copy each verdict
 * maps to. A wrong pairing would tell a person the opposite of the truth about
 * their own phone.
 */
class ComposerReachCopyTest {
    private val theirCard = ContactDelivery.OwnMailbox("relay.example")

    @Test
    fun everyVerdictButFineSaysSomething() {
        assertNull(ComposerReachCopy.stringResFor(ComposerReach.FINE))
        for (reach in ComposerReach.entries.filter { it != ComposerReach.FINE }) {
            assertNotNull("no copy for $reach", ComposerReachCopy.stringResFor(reach))
        }
    }

    @Test
    fun eachVerdictHasItsOwnLine() {
        val resIds = ComposerReach.entries.mapNotNull { ComposerReachCopy.stringResFor(it) }
        assertEquals(resIds.size, resIds.toSet().size)
    }

    @Test
    fun withoutAPassOfOurOwnTheComposerWarnsAboutReplies() {
        val reach = composerReach(
            delivery = theirCard,
            ownRelayConfigured = false,
            contactNearby = false,
            addedWhileNearby = false,
        )

        assertEquals(ComposerReach.REPLIES_CANNOT_REACH_ME, reach)
        assertNotNull(ComposerReachCopy.stringResFor(reach))
    }

    @Test
    fun aLiveLinkToTheContactKeepsTheComposerQuiet() {
        val reach = composerReach(
            delivery = ContactDelivery.NearbyOnly,
            ownRelayConfigured = false,
            contactNearby = true,
            addedWhileNearby = false,
        )

        assertEquals(ComposerReach.FINE, reach)
        assertNull(ComposerReachCopy.stringResFor(reach))
    }

    @Test
    fun aPassHolderIsToldWhenTheContactHasNoMailbox() {
        val reach = composerReach(
            delivery = ContactDelivery.NearbyOnly,
            ownRelayConfigured = true,
            contactNearby = false,
            addedWhileNearby = false,
        )

        assertEquals(ComposerReach.THEY_CANNOT_BE_REACHED, reach)
    }

    @Test
    fun meetingInPersonKeepsTheComposerQuiet() {
        val reach = composerReach(
            delivery = ContactDelivery.NearbyOnly,
            ownRelayConfigured = false,
            contactNearby = false,
            addedWhileNearby = true,
        )

        assertEquals(ComposerReach.FINE, reach)
    }
}
