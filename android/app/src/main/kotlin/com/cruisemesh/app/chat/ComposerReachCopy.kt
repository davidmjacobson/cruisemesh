package com.cruisemesh.app.chat

import com.cruisemesh.app.R
import uniffi.cruisemesh_core.ComposerReach

/**
 * Which line the composer shows for a one-to-one chat where delivery cannot
 * cross the internet in one (or both) directions. Core decides *whether* to
 * speak ([uniffi.cruisemesh_core.composerReach]); this decides what is said.
 *
 * Kept as a pure resource-id mapping with no Android imports beyond [R] so the
 * pairing is pinned by a JVM test -- the copy is the whole feature here, and a
 * wrong pairing tells someone the opposite of the truth about their own phone.
 */
object ComposerReachCopy {
    /**
     * Format-string resource for [reach], or null when the composer stays
     * silent. Every non-null string takes the contact's display name as `%1$s`.
     */
    fun stringResFor(reach: ComposerReach): Int? = when (reach) {
        ComposerReach.FINE -> null
        ComposerReach.REPLIES_CANNOT_REACH_ME -> R.string.ui_composer_replies_cannot_reach_you
        ComposerReach.THEY_CANNOT_BE_REACHED -> R.string.ui_composer_they_cannot_be_reached
        ComposerReach.NEITHER_DIRECTION_WORKS -> R.string.ui_composer_neither_direction_works
    }
}
