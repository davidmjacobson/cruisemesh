package com.cruisemesh.app.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.isToggleable
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.cruisemesh.app.mesh.RelayHealth
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class SettingsUiTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `mesh running is one labeled toggle and changes durable intent callback`() {
        var enabled by mutableStateOf(true)
        var changes = 0

        compose.setContent {
            CruiseMeshTheme {
                SettingsScreen(
                    meshEnabled = enabled,
                    meshStatus = "Mesh running",
                    relayHealth = RelayHealth.NoConfig,
                    appearancePreference = AppearancePreference.SYSTEM,
                    onAppearancePreferenceChange = {},
                    onShorePass = {},
                    onSailChecklist = {},
                    onConnectionDetails = {},
                    onDeveloperSettings = {},
                    onBackUp = {},
                    onMeshEnabledChange = {
                        enabled = it
                        changes += 1
                    },
                    onFriendsOfFriendsChanged = {},
                    onBack = {},
                )
            }
        }

        // Scrolled to first: Settings is a long scrolling screen, so where the
        // row happens to sit is not what this test is about.
        val meshToggle = compose.onNode(hasText("Mesh running") and isToggleable())
        meshToggle.performScrollTo().assertIsOn().performClick()
        meshToggle.assertIsOff()
        assertEquals(1, changes)
    }

    @Test
    fun `appearance choice is selected and reports changes`() {
        var appearance by mutableStateOf(AppearancePreference.SYSTEM)

        compose.setContent {
            CruiseMeshTheme(appearance = appearance) {
                SettingsScreen(
                    meshEnabled = true,
                    meshStatus = "Mesh running",
                    relayHealth = RelayHealth.NoConfig,
                    appearancePreference = appearance,
                    onAppearancePreferenceChange = { appearance = it },
                    onShorePass = {},
                    onSailChecklist = {},
                    onConnectionDetails = {},
                    onDeveloperSettings = {},
                    onBackUp = {},
                    onMeshEnabledChange = {},
                    onFriendsOfFriendsChanged = {},
                    onBack = {},
                )
            }
        }

        compose.onNodeWithText("Dark").performScrollTo().performClick().assertIsSelected()
        assertEquals(AppearancePreference.DARK, appearance)
    }
}
