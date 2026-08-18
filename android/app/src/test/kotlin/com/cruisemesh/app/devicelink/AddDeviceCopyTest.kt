package com.cruisemesh.app.devicelink

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.CoreLinkImportReadiness
import uniffi.cruisemesh_core.CoreLinkOutcome

/**
 * Every ending §9's ceremony can reach has a line a family can read.
 *
 * This is the test that catches a core enum gaining a variant: the `when`s in
 * `AddDeviceScreen.kt` are exhaustive, so a new outcome breaks the build rather
 * than shipping silently — and these assertions make sure the ones that exist
 * are distinct sentences rather than the same one repeated.
 */
class AddDeviceCopyTest {

    @Test
    fun `every outcome except the mid-run one has its own sentence`() {
        val terminal = CoreLinkOutcome.values().filterNot { it == CoreLinkOutcome.CHANNEL_READY }
        val copies = terminal.map { outcomeCopy(it) }

        assertEquals(terminal.size, copies.filterNotNull().size)
        assertEquals(copies.size, copies.toSet().size)
    }

    @Test
    fun `a channel that came up says nothing, because the run kept going`() {
        assertNull(outcomeCopy(CoreLinkOutcome.CHANNEL_READY))
        assertNull(outcomeCopy(null))
    }

    @Test
    fun `every step of the run has a line`() {
        val copies = LinkStep.values().map { stepCopy(it) }

        assertEquals(LinkStep.values().size, copies.size)
        // Idle and waiting deliberately share one line: from the person's side
        // there is no difference between "not started" and "waiting for the
        // other phone", and inventing one would be inventing a step.
        assertEquals(stepCopy(LinkStep.IDLE), stepCopy(LinkStep.WAITING_FOR_PEER))
    }

    @Test
    fun `a phone that is not fresh is told which kind of not-fresh it is`() {
        assertNotNull(readinessCopy(CoreLinkImportReadiness.STORE_HOLDS_SOMEONE))
        assertEquals(
            // Someone else's phone is a different sentence from your own
            // already-set-up phone: one is "use a fresh one", the other is
            // "this belongs to somebody".
            2,
            setOf(
                readinessCopy(CoreLinkImportReadiness.STORE_HOLDS_SOMEONE),
                readinessCopy(CoreLinkImportReadiness.STORE_HOLDS_ANOTHER_PERSON),
            ).size,
        )
    }

    @Test
    fun `every refusal a removal can end in has a sentence, and none repeat a wrong one`() {
        val copies = RemoveDeviceRefusal.values().map { refusalCopy(it) }

        assertEquals(RemoveDeviceRefusal.values().size, copies.size)
        // NO_DEVICES and NO_DEVICE_KEYS are the same situation seen from two
        // sides (nothing to remove from), so they share a line on purpose.
        assertEquals(
            refusalCopy(RemoveDeviceRefusal.NO_DEVICES),
            refusalCopy(RemoveDeviceRefusal.NO_DEVICE_KEYS),
        )
        assertEquals(RemoveDeviceRefusal.values().size - 1, copies.toSet().size)
    }

    @Test
    fun `every reason Remove is withheld can be explained`() {
        val copies = RemoveDeviceBlock.values().map { removeBlockCopy(it) }

        assertEquals(RemoveDeviceBlock.values().size, copies.toSet().size)
    }
}
