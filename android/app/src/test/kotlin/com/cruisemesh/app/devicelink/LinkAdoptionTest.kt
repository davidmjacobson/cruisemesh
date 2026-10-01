package com.cruisemesh.app.devicelink

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.cruisemesh.app.identity.OnboardingStore
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.cruisemesh_core.LinkBootstrapProfile

/**
 * The person's own name and photo crossing §9's export, both ends.
 *
 * The bug this pins is not subtle once stated: the link export carried no
 * profile at all, so a phone that had just been adopted asked the person their
 * own name — and any answer was a second name for one person, which is a fleet
 * profile fork nothing in v1 reconciles.
 */
@RunWith(RobolectricTestRunner::class)
class LinkAdoptionTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Before
    @After
    fun clearStores() {
        context.getSharedPreferences("cruisemesh_profile", Context.MODE_PRIVATE)
            .edit().clear().commit()
        context.getSharedPreferences("cruisemesh_onboarding", Context.MODE_PRIVATE)
            .edit().clear().commit()
        ProfilePhotoStore.clear(context)
    }

    @Test
    fun `the approving device sends the name its contacts already see`() {
        ProfileStore.saveDisplayName(context, "Maya", durable = true)
        ProfilePhotoStore.restoreBackupBytes(context, PHOTO)
        ProfileStore.restoreOwnAvatarEpoch(context, EPOCH)

        val profile = LinkAdoption.profileOf(context)

        assertEquals("Maya", profile.displayName)
        assertArrayEquals(PHOTO, profile.avatar)
        assertEquals(EPOCH, profile.avatarEpoch)
    }

    @Test
    fun `a person who never chose a name still sends the one they are shown under`() {
        // Not empty and not null: this is the name this person's contacts see
        // today, so a phone joining them must not disagree about it.
        assertEquals(ProfileStore.defaultDisplayName(), LinkAdoption.profileOf(context).displayName)
    }

    @Test
    fun `an adopted phone takes the profile and stops being unset-up`() {
        LinkAdoption.adopted(
            context,
            LinkBootstrapProfile(displayName = "Maya", avatar = PHOTO, avatarEpoch = EPOCH),
        )

        assertEquals("Maya", ProfileStore.loadStoredDisplayName(context))
        assertArrayEquals(PHOTO, ProfilePhotoStore.loadBackupBytes(context))
        // Restored, never bumped: this is the number profile sync orders
        // updates by, so a fresh one here would make the newest phone outrank
        // the fleet it just joined and re-broadcast the person's profile.
        assertEquals(EPOCH, ProfileStore.loadOwnAvatarEpoch(context))
        // And the field the person was being asked to fill in is filled in, so
        // first-run setup has nothing left to ask.
        assertTrue(OnboardingStore.isCompleted(context))
        assertTrue(ProfileStore.loadStoredDisplayName(context).isNotEmpty())
        // ...except the one thing this route went around. The wizard carries
        // the permissions step; an adopted phone never saw it, and used to land
        // on the chat list with the mesh off behind a "Permissions required"
        // notice. `FirstRunRouter` collects this on the way in.
        assertEquals(false, OnboardingStore.permissionsStepDone(context))
    }

    @Test
    fun `an export with no name leaves the question open rather than blanking it`() {
        ProfileStore.saveDisplayName(context, "Maya", durable = true)

        LinkAdoption.adopted(
            context,
            LinkBootstrapProfile(displayName = null, avatar = ByteArray(0), avatarEpoch = 0L),
        )

        assertEquals("Maya", ProfileStore.loadStoredDisplayName(context))
        assertTrue(OnboardingStore.isCompleted(context))
    }

    private companion object {
        val PHOTO = ByteArray(64) { it.toByte() }
        const val EPOCH = 1_755_000_000_000L
    }
}
