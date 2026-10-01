package com.cruisemesh.app.relay

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The relay token is a bearer credential and the diagnostics log gets emailed
 * to whoever is helping. These pin the one place the pass is written down: a
 * digest of the token, never characters of it.
 */
class RelayConfigStoreTokenFingerprintTest {

    @Test
    fun `the same pass always gets the same label`() {
        // The whole reason to log anything: two lines, or two sessions, about
        // one pass have to be recognisable as the same pass.
        for (token in listOf(HEX_TOKEN, FAMILY_TOKEN)) {
            assertEquals(
                RelayConfigStore.tokenFingerprint(token),
                RelayConfigStore.tokenFingerprint(token),
            )
        }
    }

    @Test
    fun `no run of the token survives into the label`() {
        // What separates a digest from a truncation, and the property a
        // future "just shorten it" refactor would quietly break.
        for (token in listOf(HEX_TOKEN, FAMILY_TOKEN)) {
            val fingerprint = RelayConfigStore.tokenFingerprint(token)
            for (width in 2..token.length) {
                for (start in 0..token.length - width) {
                    val run = token.substring(start, start + width)
                    assertTrue(
                        "fingerprint $fingerprint contains token run $run",
                        !fingerprint.contains(run),
                    )
                }
            }
        }
    }

    @Test
    fun `two different passes stay distinguishable`() {
        // Telling a household's own pass apart from the shared tester pass in
        // a support hand-off.
        assertNotEquals(
            RelayConfigStore.tokenFingerprint(HEX_TOKEN),
            RelayConfigStore.tokenFingerprint(FAMILY_TOKEN),
        )
        // A pass that differs in one character has to land somewhere else
        // entirely; a prefix would have printed the same eight characters.
        assertNotEquals(
            RelayConfigStore.tokenFingerprint(HEX_TOKEN),
            RelayConfigStore.tokenFingerprint(HEX_TOKEN.dropLast(1) + "9"),
        )
    }

    @Test
    fun `the label matches the value iOS derives`() {
        // Pinned in the core's own tests too. Restated here because the point
        // of putting the derivation in the core is that a support person
        // comparing an Android archive against an iPhone's sees one pass named
        // once -- and without this, nothing here would fail if that stopped
        // being true.
        assertEquals("056855d3", RelayConfigStore.tokenFingerprint(HEX_TOKEN))
        assertEquals("6ae48e6b", RelayConfigStore.tokenFingerprint(FAMILY_TOKEN))
    }

    @Test
    fun `a short or empty token does not blow up`() {
        assertEquals(8, RelayConfigStore.tokenFingerprint("abc").length)
        assertEquals(8, RelayConfigStore.tokenFingerprint("").length)
    }

    private companion object {
        const val HEX_TOKEN = "4ac9f24f8b1e4d7fae0c3b19d6725f88"
        const val FAMILY_TOKEN = "cmfam1-9d41c0b7e2a54f16"
    }
}
