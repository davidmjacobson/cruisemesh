package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.voiceCaptureIdleState

/**
 * The composer renders its starting state without calling the core, because the
 * Compose preview screenshot renderer inflates it in a sandbox with no native
 * library. That is a rendering concession, not a second opinion about what
 * "not recording" means — this keeps the two in step.
 */
class MessageComposerCoreContractTest {
    @Test
    fun `the composer's starting state is the core's idle state`() {
        assertEquals(voiceCaptureIdleState(), IDLE_VOICE_CAPTURE)
    }
}
