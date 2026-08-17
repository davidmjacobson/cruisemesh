package com.cruisemesh.app.ui

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.R
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class OnboardingUiTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun allPagesLeadToANameGatedCompletion() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val next = context.getString(R.string.ui_next)
        val start = context.getString(R.string.ui_start_using_cruisemesh)
        var displayName by mutableStateOf("")
        var completions = 0

        compose.setContent {
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
                    onComplete = { completions += 1 },
                )
            }
        }

        // One Next per page before the profile slide: welcome, delivery,
        // Shore Pass, permissions, Wi-Fi.
        repeat(5) {
            compose.onNodeWithText(next).performClick()
            compose.waitForIdle()
        }
        compose.onNodeWithText(start).assertIsNotEnabled()
        compose.onNode(hasSetTextAction()).performTextInput("Maya")
        compose.onNodeWithText(start).assertIsEnabled().performClick()

        assertEquals(1, completions)
    }
}
