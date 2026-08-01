package com.cruisemesh.app.friending

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.Contact

class ContactsScreenDisplayNameTest {
    @Test
    fun `nickname replaces friend card name in New Chat`() {
        val contact = Contact(
            userId = ByteArray(16) { it.toByte() },
            name = "Katherine",
            signPk = ByteArray(32),
            agreePk = ByteArray(32),
            relayUrl = null,
            relayToken = null,
            nickname = "Katie",
        )

        assertEquals("Katie", contactsScreenDisplayName(contact))
    }
}
