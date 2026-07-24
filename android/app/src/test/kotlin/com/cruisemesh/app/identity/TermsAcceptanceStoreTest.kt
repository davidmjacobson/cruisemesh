package com.cruisemesh.app.identity

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TermsAcceptanceStoreTest {
    @Test
    fun `only the current terms version is accepted`() {
        assertTrue(isCurrentTermsVersion(CURRENT_TERMS_VERSION))
        assertFalse(isCurrentTermsVersion(null))
        assertFalse(isCurrentTermsVersion("2026-07-22"))
        assertFalse(isCurrentTermsVersion("accepted"))
    }
}
