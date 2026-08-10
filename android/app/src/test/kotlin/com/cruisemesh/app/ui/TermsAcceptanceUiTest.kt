package com.cruisemesh.app.ui

import android.content.Context
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.assertIsToggleable
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.R
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class TermsAcceptanceUiTest {
    @get:Rule
    val compose = createComposeRule()

    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test
    fun acceptRequiresConfirmationAndFiresOnce() {
        var accepted = 0
        compose.setContent {
            CruiseMeshTheme {
                TermsAcceptanceScreen { accepted += 1 }
            }
        }

        val confirmation = context.getString(R.string.ui_terms_acceptance_confirmation)
        val agree = context.getString(R.string.ui_i_agree)

        compose.onNodeWithTag(UiTestTags.TERMS_SCREEN).assertIsDisplayed()
        compose.onNodeWithText(agree).assertIsNotEnabled()
        compose.onAllNodesWithText(confirmation).assertCountEquals(1)
        compose.onNodeWithText(confirmation)
            .assertIsToggleable()
            .assertIsOff()
            .performClick()
            .assertIsOn()
        compose.onNodeWithText(agree).assertIsEnabled().performClick()

        assertEquals(1, accepted)
    }
}
