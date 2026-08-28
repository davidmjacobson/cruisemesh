package com.cruisemesh.app.identity

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The permissions-step flag has three states, and the third one is the whole
 * point of it.
 *
 * A phone installed before the flag existed has finished setup and must never
 * be dragged back through it by an app update; a phone adopted or restored by a
 * door that went around the wizard has finished setup and still owes the step.
 * Two booleans cannot tell those apart, so "never recorded" has to survive as
 * its own answer rather than collapsing into the default.
 */
@RunWith(RobolectricTestRunner::class)
class OnboardingStoreTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Before
    @After
    fun clearStore() {
        context.getSharedPreferences("cruisemesh_onboarding", Context.MODE_PRIVATE)
            .edit().clear().commit()
    }

    @Test
    fun `an install that never recorded an answer reads as no answer`() {
        assertNull(OnboardingStore.permissionsStepDone(context))
    }

    @Test
    fun `an install older than the flag still reads as no answer`() {
        // Completing setup is the only thing such an install ever wrote here.
        OnboardingStore.markCompleted(context)

        assertNull(OnboardingStore.permissionsStepDone(context))
    }

    @Test
    fun `a route that skipped the step records that it is owed`() {
        OnboardingStore.markPermissionsStepPending(context)

        assertEquals(false, OnboardingStore.permissionsStepDone(context))
    }

    @Test
    fun `walking through the step clears the debt`() {
        OnboardingStore.markPermissionsStepPending(context)
        OnboardingStore.markPermissionsStepDone(context)

        assertEquals(true, OnboardingStore.permissionsStepDone(context))
    }
}
