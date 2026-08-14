package com.cruisemesh.app.friending

import com.cruisemesh.app.R
import uniffi.cruisemesh_core.CoreException

/**
 * Map a failed friend-card / deep-link parse to the sentence the user should
 * read. A newer scheme than this build implements
 * ([CoreException.UnsupportedLink]) is "update the app", never a crash and
 * never a half-parsed contact (`specs/multi-device-v1.md` WPT).
 */
fun friendImportFailureResId(error: Throwable, text: String): Int = when (error) {
    is CoreException.UnsupportedLink -> R.string.ui_this_link_needs_a_newer_version
    else -> if (text.contains("CMFRIEND") || text.contains("CMLINK")) {
        R.string.ui_that_looks_like_a_friend_card_but_part
    } else {
        R.string.ui_not_a_cruisemesh_friend_card
    }
}
