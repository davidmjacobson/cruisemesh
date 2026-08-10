package com.cruisemesh.app.identity

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TermsAcceptanceStoreTest {
    @Test
    fun `only the current terms version is accepted`() {
        assertEquals("2026-08-08", CURRENT_TERMS_VERSION)
        assertTrue(isCurrentTermsVersion(CURRENT_TERMS_VERSION))
        assertFalse(isCurrentTermsVersion(null))
        assertFalse(isCurrentTermsVersion("2026-07-23"))
        assertFalse(isCurrentTermsVersion("accepted"))
    }
}
