package com.cruisemesh.app.ui

import android.content.Context
import android.view.View
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.dp
import androidx.core.graphics.Insets
import androidx.core.view.WindowInsetsCompat
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.R
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Field session 2026-08-24, Pixel 10 Pro at font +3 / display +1: the wizard
 * cards clip their overflow with no scroll affordance, so the permission
 * grant buttons and the display name field sat below the fold — testers
 * finished onboarding with no permissions and typed their name under the
 * keyboard (the app is edge-to-edge, so adjustResize resizes nothing and the
 * IME must be consumed by the layout itself, same as the chat composer arc).
 * These pin the two fixes: actionable controls come before the explainer
 * content, and the bottom bar rises with the keyboard.
 */
@RunWith(AndroidJUnit4::class)
class OnboardingLargeFontUiTest {
    @get:Rule
    val compose = createComposeRule()

    private val context: Context = ApplicationProvider.getApplicationContext()
    private var hostView: View? = null

    private fun setScreenContent() {
        var displayName by mutableStateOf("")
        compose.setContent {
            hostView = LocalView.current
            CruiseMeshTheme {
                OnboardingScreen(
                    userId = ByteArray(32) { 1 },
                    displayId = "CM-K7QX-9M2P-3F8J-QRTZ-AB",
                    displayName = displayName,
                    avatarPath = null,
                    meshPermissionsGranted = false,
                    notificationPermissionGranted = false,
                    batteryExemptionGranted = false,
                    onDisplayNameChange = { displayName = it },
                    onTakePhoto = {},
                    onChoosePhoto = {},
                    onRemovePhoto = {},
                    onRequestMeshPermissions = {},
                    onRequestNotificationPermission = {},
                    onRequestBatteryExemption = {},
                    onRestore = {},
                    onComplete = {},
                )
            }
        }
    }

    private fun next(times: Int) {
        val next = context.getString(R.string.ui_next)
        repeat(times) {
            compose.onNodeWithText(next).performClick()
            compose.waitForIdle()
        }
    }

    private fun dispatchBottomIme(imePx: Int) {
        val insets = WindowInsetsCompat.Builder()
            .setInsets(WindowInsetsCompat.Type.ime(), Insets.of(0, 0, 0, imePx))
            .setVisible(WindowInsetsCompat.Type.ime(), imePx > 0)
            .build()
            .toWindowInsets()!!
        compose.runOnUiThread {
            checkNotNull(hostView).dispatchApplyWindowInsets(insets)
        }
        compose.waitForIdle()
    }

    @Test
    fun grantButtonsPrecedeTheExplainerCards() {
        setScreenContent()
        next(3)

        val buttonTop = compose
            .onNodeWithText(context.getString(R.string.ui_enable_nearby_access_required))
            .getUnclippedBoundsInRoot().top
        val cardTop = compose
            .onNodeWithText(context.getString(R.string.ui_onboarding_permission_nearby_title))
            .getUnclippedBoundsInRoot().top
        assertTrue(
            "the grant button ($buttonTop) must lay out above the explainer card " +
                "($cardTop): below it, large-font users never see it and skip the step",
            buttonTop < cardTop,
        )
    }

    @Test
    fun nameFieldPrecedesThePhotoControls() {
        setScreenContent()
        next(5)

        val nameTop = compose.onNode(hasSetTextAction()).getUnclippedBoundsInRoot().top
        val takePhotoTop = compose
            .onNodeWithText(context.getString(R.string.ui_take_photo))
            .getUnclippedBoundsInRoot().top
        assertTrue(
            "the required name field ($nameTop) must lay out above the optional photo " +
                "controls ($takePhotoTop)",
            nameTop < takePhotoTop,
        )
    }

    @Test
    fun bottomBarRisesAboveTheKeyboard() {
        setScreenContent()
        next(5)
        compose.waitForIdle()

        val rootBottom = compose.onRoot().getUnclippedBoundsInRoot().bottom
        val start = context.getString(R.string.ui_start_using_cruisemesh)

        dispatchBottomIme(imePx = 300)

        // Robolectric's default density is mdpi, so 300 px == 300.dp.
        val raisedBottom = compose.onNodeWithText(start).getUnclippedBoundsInRoot().bottom
        assertTrue(
            "the bottom bar sits at $raisedBottom; with a 300px keyboard it must clear " +
                "$rootBottom - 300dp, or the keyboard covers the bar and the name field",
            raisedBottom <= rootBottom - 300.dp,
        )
    }
}
