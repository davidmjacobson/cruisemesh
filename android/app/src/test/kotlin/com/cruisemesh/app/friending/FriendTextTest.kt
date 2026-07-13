package com.cruisemesh.app.friending

import org.junit.Assert.assertEquals
import org.junit.Test

class FriendTextTest {
    @Test
    fun extractsTokenFromProse() {
        assertEquals(
            "CMFRIEND1:abc123",
            extractFriendToken("Add me:\nCMFRIEND1:abc123"),
        )
    }

    @Test
    fun extractsFirstToken() {
        assertEquals(
            "CMFRIEND1:first",
            extractFriendToken("CMFRIEND1:first CMFRIEND1:second"),
        )
    }

    @Test
    fun returnsTrimmedInputWhenNoToken() {
        assertEquals("""{"name":"A"}""", extractFriendToken("  {\"name\":\"A\"} \n"))
    }

    @Test
    fun stripsTrailingPunctuationFromToken() {
        assertEquals(
            "CMFRIEND1:abc_123-xyz",
            extractFriendToken("Paste it: CMFRIEND1:abc_123-xyz."),
        )
    }
}
