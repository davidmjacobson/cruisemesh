package com.cruisemesh.app.friending

import com.cruisemesh.app.R
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.CoreException

class FriendImportFailureTest {

    @Test
    fun `a future link scheme asks the user to update the app`() {
        assertEquals(
            R.string.ui_this_link_needs_a_newer_version,
            friendImportFailureResId(CoreException.UnsupportedLink(), "CMFRIEND5:abc"),
        )
        assertEquals(
            R.string.ui_this_link_needs_a_newer_version,
            friendImportFailureResId(CoreException.UnsupportedLink(), "CMLINK1:abc"),
        )
    }

    @Test
    fun `a truncated known card is not treated as unknown junk`() {
        assertEquals(
            R.string.ui_that_looks_like_a_friend_card_but_part,
            friendImportFailureResId(
                CoreException.InvalidFriendCard("truncated"),
                "CMFRIEND3:abc",
            ),
        )
    }

    @Test
    fun `unrelated paste stays a generic not-a-card message`() {
        assertEquals(
            R.string.ui_not_a_cruisemesh_friend_card,
            friendImportFailureResId(RuntimeException("nope"), "hello"),
        )
    }
}
