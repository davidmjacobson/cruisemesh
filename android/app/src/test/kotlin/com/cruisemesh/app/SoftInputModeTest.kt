package com.cruisemesh.app

import android.content.ComponentName
import android.content.Context
import android.view.WindowManager
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The conversation's top bar has been pinned in Scaffold's `topBar` slot for
 * a long time, and a composable-level test already asserts it survives the
 * keyboard. Neither reaches the thing that actually moved it: with no
 * `windowSoftInputMode` declared, the *window* is free to pan, taking the
 * pinned bar off the top of the screen with it. That is a manifest property,
 * so it takes a manifest test.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SoftInputModeTest {

    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    @Test
    fun `the main activity resizes for the keyboard instead of panning`() {
        val activity = ComponentName(context, MainActivity::class.java)
        val declared = context.packageManager.getActivityInfo(activity, 0).softInputMode

        assertEquals(
            "MainActivity must declare adjustResize: panning slides the chat's" +
                " contact header off-screen when the keyboard opens.",
            WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE,
            declared and WindowManager.LayoutParams.SOFT_INPUT_MASK_ADJUST,
        )
    }
}
