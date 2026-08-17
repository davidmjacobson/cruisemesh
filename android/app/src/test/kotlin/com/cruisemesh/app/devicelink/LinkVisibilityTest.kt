package com.cruisemesh.app.devicelink

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.MessageStore

/**
 * The shell half of §9.4's "may not advertise ANYTHING".
 *
 * Core refuses everything it holds during the pre-activation window, and
 * `core/tests/device_link_activation.rs` proves it by asking a pre-activation
 * store to author, ack, publish hints and offer carry, and watching it refuse
 * all four. None of that reaches a BLE advertiser or an NSD registration, which
 * are this shell's to hold shut -- so this pins the one wire between the two:
 * the moment the window opens, the answer the mesh service consults flips, and
 * the service is told without waiting for a tick.
 */
class LinkVisibilityTest {

    @After
    fun tearDown() {
        // The gate is a process-wide singleton, so a test that left it silent
        // would silence the next one. Put it back the way a fresh install
        // reads: nobody listening, everything permitted, nothing pending.
        LinkVisibility.unregister()
        LinkVisibility.refresh(MessageStore.open(":memory:"))
    }

    @Test
    fun anInstallThatHasNeverLinkedMayAdvertise() {
        val store = MessageStore.open(":memory:")
        LinkVisibility.refresh(store)
        assertTrue(LinkVisibility.mayAdvertise())
    }

    @Test
    fun openingThePreActivationWindowTakesThisDeviceOffTheAir() {
        val store = MessageStore.open(":memory:")
        LinkVisibility.refresh(store)

        val told = mutableListOf<Boolean>()
        LinkVisibility.register { told += it }
        assertEquals(listOf(true), told)

        // §9.4(a): the channel is confirmed and the export has not landed yet.
        store.beginLinkActivation(BINDING, NOW)
        LinkVisibility.refresh(store)

        assertFalse("a pre-activation device may not advertise", LinkVisibility.mayAdvertise())
        assertEquals(
            "the mesh service must be told the moment the window opens",
            listOf(true, false),
            told,
        )

        // Idempotent: re-reading the same stage is not a second event, so the
        // service's periodic tick cannot flap the radios.
        LinkVisibility.refresh(store)
        assertEquals(listOf(true, false), told)
    }

    /**
     * The change is a request, not a fact: the mesh service applies it on its
     * own handler one post later. The new device must not put its first frame
     * on the wire until the radios have actually gone down, so the wait is the
     * wire between the two -- and it must not hang when nobody is listening.
     */
    @Test
    fun theWaitTracksWhatTheServiceDidRatherThanWhatItWasTold() {
        val store = MessageStore.open(":memory:")
        LinkVisibility.refresh(store)

        // A listener that defers, exactly as the mesh service's handler post
        // does. The answer has flipped; nothing has been applied.
        val deferred = mutableListOf<Boolean>()
        LinkVisibility.register { deferred += it }
        store.beginLinkActivation(BINDING, NOW)
        LinkVisibility.refresh(store)
        assertFalse(LinkVisibility.mayAdvertise())
        assertFalse(
            "the radios are not down until the service says so",
            LinkVisibility.awaitApplied(target = false, timeoutMs = 50),
        )

        // The service reports back, and the waiter is released.
        LinkVisibility.markApplied(false)
        assertTrue(LinkVisibility.awaitApplied(target = false, timeoutMs = 50))

        // With nobody registered there is nothing to wait for, so a refresh
        // must not leave a caller blocking on a service that is not running.
        LinkVisibility.unregister()
        val fresh = MessageStore.open(":memory:")
        LinkVisibility.refresh(fresh)
        assertTrue(LinkVisibility.mayAdvertise())
        assertTrue(LinkVisibility.awaitApplied(target = true, timeoutMs = 50))
    }

    private companion object {
        const val NOW = 1_755_000_000_000L
        val BINDING = ByteArray(32) { 0x7C }
    }
}
