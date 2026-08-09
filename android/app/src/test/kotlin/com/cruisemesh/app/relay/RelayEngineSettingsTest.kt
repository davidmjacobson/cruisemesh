package com.cruisemesh.app.relay

import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The rollback switch, and the two things that matter about it: what it says
 * when nobody has touched it, and that it is only ever read once per pass.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RelayEngineSettingsTest {

    private val context get() = ApplicationProvider.getApplicationContext<android.content.Context>()

    @Test
    fun `a device that has never been told anything runs the legacy engine`() {
        // The default is the whole safety property of this package: with
        // nothing set, nothing about a relay pass changes.
        assertEquals(RelayPassEngine.LEGACY, RelayEngineSettings.passEngine(context))
    }

    @Test
    fun `the canary is on by default, because one nobody switches on is not a canary`() {
        assertTrue(RelayEngineSettings.shadowEnabled(context))
    }

    @Test
    fun `the selection round trips and can be put back`() {
        RelayEngineSettings.setPassEngine(context, RelayPassEngine.CORE)
        assertEquals(RelayPassEngine.CORE, RelayEngineSettings.passEngine(context))
        // Rollback is a preference write, not a migration: there is no schema
        // to move and nothing to undo in the store.
        RelayEngineSettings.setPassEngine(context, RelayPassEngine.LEGACY)
        assertEquals(RelayPassEngine.LEGACY, RelayEngineSettings.passEngine(context))
    }

    @Test
    fun `turning the canary off leaves the engine alone`() {
        RelayEngineSettings.setShadowEnabled(context, false)
        assertFalse(RelayEngineSettings.shadowEnabled(context))
        assertEquals(RelayPassEngine.LEGACY, RelayEngineSettings.passEngine(context))
        assertFalse(relayShadowPermitted(RelayPassEngine.LEGACY, RelayEngineSettings.shadowEnabled(context)))
    }

    @Test
    fun `the selection never touches the message store, so deleting it needs no migration`() {
        // C5 removes this switch. That is only cheap if it never earned a
        // column: store schemas here are forward-only, so a flag that lived in
        // one would leave either a dead column forever or a migration that
        // drops nothing. Nothing in this object can reach the store.
        val storeTypes = RelayEngineSettings::class.java.declaredMethods
            .flatMap { it.parameterTypes.toList() + it.returnType }
            .map { it.name }
        assertFalse(
            "the engine selection must not be able to reach the message store",
            storeTypes.any { it.contains("MessageStore") },
        )
    }

    @Test
    fun `the flag lives beside the other relay settings and disturbs none of them`() {
        RelayConfigStore.save(context, "https://relay.example", "member-token")
        RelayEngineSettings.setPassEngine(context, RelayPassEngine.CORE)
        RelayEngineSettings.setShadowEnabled(context, false)

        val config = RelayConfigStore.load(context)
        assertEquals("https://relay.example", config?.relayUrl)
        assertEquals("member-token", config?.relayToken)
        assertTrue(RelayConfigStore.shareOnline(context))
    }
}
