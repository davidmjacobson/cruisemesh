package com.cruisemesh.app.devicelink

import android.content.Context
import com.cruisemesh.app.identity.OnboardingStore
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import uniffi.cruisemesh_core.LinkBootstrapProfile

/**
 * The person's own name and photo, crossing §9's export.
 *
 * Core carries them now ([LinkBootstrapProfile], `bootstrap.rs`) and keeps no
 * profile row of its own, so the two stores that actually hold them are this
 * shell's — which makes this file the whole of the Android side: read them on
 * the phone that is doing the adopting, write them on the phone being adopted.
 *
 * # Why this is not the shell inventing a policy
 *
 * It is the restore path's three writes, unchanged: `BackupService.restoreBackup`
 * installs display name, photo and photo revision from an authenticated `.cmbak`
 * and marks setup complete, and §9's closing paragraph says the two doors out of
 * first-run setup must land a person in the same place. Before this, one of them
 * asked a linked person their own name — and any answer they typed was a second
 * name for one person, which is the fleet-wide profile fork WP4's settings
 * stream would then have had to reconcile.
 *
 * The photo revision is *restored*, never bumped: it is the number profile sync
 * orders updates by, so minting a fresh one here would make a phone that has
 * just been adopted outrank the fleet it joined and re-broadcast the person's
 * profile from the newest device.
 */
internal object LinkAdoption {

    /**
     * What the approving device sends. [ProfileStore.loadDisplayName], not the
     * stored-only reader, for the same reason the backup uses it: this is the
     * name this person's contacts already see, including the fallback a profile
     * that predates the name requirement is shown under.
     */
    fun profileOf(context: Context): LinkBootstrapProfile = LinkBootstrapProfile(
        displayName = ProfileStore.loadDisplayName(context),
        avatar = ProfilePhotoStore.loadBackupBytes(context),
        avatarEpoch = ProfileStore.loadOwnAvatarEpoch(context),
    )

    /**
     * What the newly adopted device does with it, called once §9.4's
     * acknowledgement has completed and never before: a ceremony that failed
     * must leave this phone exactly as it found it.
     *
     * Marking setup complete belongs here rather than only in the navigation
     * callback because it is a fact about the store, not about a screen: this
     * phone now holds a person's contacts and history whether or not anybody
     * taps Done afterwards.
     */
    fun adopted(context: Context, profile: LinkBootstrapProfile) {
        // A blank name never replaces a real one -- and on this path there is
        // nothing to replace anyway, since only a factory-fresh phone can be
        // adopted (§9.3).
        profile.displayName?.let { ProfileStore.saveDisplayName(context, it) }
        ProfilePhotoStore.restoreBackupBytes(context, profile.avatar)
        ProfileStore.restoreOwnAvatarEpoch(context, profile.avatarEpoch)
        OnboardingStore.markCompleted(context)
    }
}
