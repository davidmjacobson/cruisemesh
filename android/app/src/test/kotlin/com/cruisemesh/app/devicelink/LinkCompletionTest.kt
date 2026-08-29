package com.cruisemesh.app.devicelink

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreLinkRole

/**
 * One "Done" button, two endings, two destinations.
 *
 * The field session on 2026-08-18 hit the first row of this table: a finished
 * adoption went back the way it came and landed on the first-run wizard, which
 * offered the link door again and then asked a linked person their own name. The
 * rows below it are the ones a naive fix breaks — the approving device belongs
 * back in "Your devices", and a run that failed is not a phone that is set up.
 */
class LinkCompletionTest {

    @Test
    fun `an adopted phone goes into the app`() {
        assertTrue(LinkCompletion.entersApp(CoreLinkRole.NEW_DEVICE, LinkStep.DONE))
    }

    @Test
    fun `a run that failed is not a phone that is set up`() {
        assertFalse(LinkCompletion.entersApp(CoreLinkRole.NEW_DEVICE, LinkStep.FAILED))
    }

    @Test
    fun `the phone that did the adopting goes back where it came from`() {
        assertFalse(LinkCompletion.entersApp(CoreLinkRole.APPROVING_DEVICE, LinkStep.DONE))
        assertFalse(LinkCompletion.entersApp(CoreLinkRole.APPROVING_DEVICE, LinkStep.FAILED))
    }

    @Test
    fun `nothing mid-run counts as an ending`() {
        val midRun = LinkStep.values().filterNot { it == LinkStep.DONE || it == LinkStep.FAILED }
        for (step in midRun) {
            assertFalse(step.name, LinkCompletion.entersApp(CoreLinkRole.NEW_DEVICE, step))
        }
    }

    /**
     * The regression this pair of rules exists for: a cancelled run kept the
     * code and the copy button on screen, so the screen that said "Stopped"
     * was at the same time inviting somebody to scan something dead.
     */
    @Test
    fun `a stopped run shows no code to scan`() {
        assertFalse(LinkCompletion.showsOffer(CoreLinkRole.NEW_DEVICE, LinkStep.FAILED))
    }

    @Test
    fun `a finished run shows no code either`() {
        assertFalse(LinkCompletion.showsOffer(CoreLinkRole.NEW_DEVICE, LinkStep.DONE))
    }

    @Test
    fun `a live run still shows its code`() {
        val live = LinkStep.values().filterNot { it == LinkStep.DONE || it == LinkStep.FAILED }
        for (step in live) {
            assertTrue(step.name, LinkCompletion.showsOffer(CoreLinkRole.NEW_DEVICE, step))
        }
    }

    /** The approving end scans an offer; it never shows one. */
    @Test
    fun `the approving device never shows a code`() {
        for (step in LinkStep.values()) {
            assertFalse(step.name, LinkCompletion.showsOffer(CoreLinkRole.APPROVING_DEVICE, step))
        }
    }

    @Test
    fun `only a stopped run offers another go`() {
        assertTrue(LinkCompletion.offersRestart(LinkStep.FAILED))
        for (step in LinkStep.values().filterNot { it == LinkStep.FAILED }) {
            assertFalse(step.name, LinkCompletion.offersRestart(step))
        }
    }
}
