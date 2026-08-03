package com.cruisemesh.app.relay

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The relay token is a bearer credential and the diagnostics log gets emailed
 * to whoever is helping. These pin the one place it is written down.
 */
class RelayConfigStoreTokenPrefixTest {

    @Test
    fun `only the first eight characters survive`() {
        val token = "4ac9f24f9e1b4c0d8a7e6f5d4c3b2a19"
        assertEquals("4ac9f24f", RelayConfigStore.tokenPrefix(token))
    }

    @Test
    fun `the full token never appears in the prefix`() {
        val token = "c106da0100000000deadbeefcafebabe"
        assertTrue(!RelayConfigStore.tokenPrefix(token).contains(token))
        assertTrue(RelayConfigStore.tokenPrefix(token).length <= 8)
    }

    @Test
    fun `two different passes stay distinguishable`() {
        // The point of logging any of it: telling a family's own pass apart
        // from the shared tester pass in a support hand-off.
        val family = "4ac9f24f9e1b4c0d"
        val tester = "c106da0111112222"
        assertTrue(RelayConfigStore.tokenPrefix(family) != RelayConfigStore.tokenPrefix(tester))
    }

    @Test
    fun `a short or empty token does not blow up`() {
        assertEquals("abc", RelayConfigStore.tokenPrefix("abc"))
        assertEquals("", RelayConfigStore.tokenPrefix(""))
    }
}
