package com.cruisemesh.app.ui

import android.content.Context
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.requiredHeight
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.R
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.cruisemesh_core.Contact

@RunWith(AndroidJUnit4::class)
class ContactDetailsSheetUiTest {
    @get:Rule
    val compose = createComposeRule()

    private val context: Context = ApplicationProvider.getApplicationContext()
    private val maya = Contact(
        userId = ByteArray(32) { 1 },
        name = "Maya",
        signPk = ByteArray(32) { 2 },
        agreePk = ByteArray(32) { 3 },
        relayUrl = null,
        relayToken = null,
    )

    // Regression: at large font/display scale the sheet content is taller
    // than the screen, and Report/Delete sat below the clipped bottom edge
    // with no way to reach them. The constrained box stands in for the small
    // viewport; performScrollTo fails unless the content is scrollable.
    @Test
    fun reportAndDeleteAreReachableWhenContentOverflows() {
        var reported = false
        var deleted = false

        compose.setContent {
            CruiseMeshTheme {
                Box(Modifier.requiredHeight(320.dp)) {
                    ContactDetailsSheetContent(
                        contact = maya,
                        connectivityText = "Trying nearby phones",
                        onReport = { reported = true },
                        onDeleteContact = { deleted = true },
                    )
                }
            }
        }

        compose.onNodeWithText(context.getString(R.string.ui_report_contact))
            .performScrollTo()
            .assertIsDisplayed()
            .performClick()
        assertTrue(reported)

        compose.onNodeWithText(context.getString(R.string.ui_delete_contact))
            .performScrollTo()
            .assertIsDisplayed()
            .performClick()
        assertTrue(deleted)
    }
}
