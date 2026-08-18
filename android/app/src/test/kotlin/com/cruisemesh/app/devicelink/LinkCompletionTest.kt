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
}
