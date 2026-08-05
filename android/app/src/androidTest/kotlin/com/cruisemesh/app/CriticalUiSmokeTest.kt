package com.cruisemesh.app

import android.content.Context
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.ui.UiTestTags
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class CriticalUiSmokeTest {
    @get:Rule
    val compose = createAndroidComposeRule<MainActivity>()

    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test
    fun coldStartMovesFromTermsToOnboardingWithoutBlanking() {
        compose.onNodeWithTag(UiTestTags.TERMS_SCREEN).assertIsDisplayed()
        compose.onNodeWithText(context.getString(R.string.ui_terms_acceptance_confirmation)).performClick()
        compose.onNodeWithText(context.getString(R.string.ui_i_agree)).performClick()
        compose.onNodeWithTag(UiTestTags.ONBOARDING_SCREEN).assertIsDisplayed()
        compose.onNodeWithText(context.getString(R.string.ui_next)).assertIsDisplayed()
    }
}
