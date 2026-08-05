package com.cruisemesh.app.ui

import android.content.Context
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
import org.junit.Assert.assertSame
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.cruisemesh_core.Contact

@RunWith(AndroidJUnit4::class)
class NewGroupUiTest {
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

    @Test
    fun createRequiresNameAndMemberThenReturnsTrimmedSelection() {
        var createdName: String? = null
        var createdMembers: List<Contact>? = null

        compose.setContent {
            CruiseMeshTheme {
                NewGroupScreen(
                    contacts = listOf(maya),
                    onCreate = { name, members ->
                        createdName = name
                        createdMembers = members
                    },
                    onBack = {},
                )
            }
        }

        val create = context.getString(R.string.ui_create_group)
        compose.onNodeWithText(create).assertIsNotEnabled()
        compose.onNode(hasSetTextAction()).performTextInput("  Excursion crew  ")
        compose.onNodeWithText(create).assertIsNotEnabled()
        compose.onNodeWithText("Maya").performClick()
        compose.onNodeWithText(
            context.resources.getQuantityString(R.plurals.ui_create_group_count, 1, 1),
        ).assertIsEnabled().performClick()

        assertEquals("Excursion crew", createdName)
        assertEquals(1, createdMembers?.size)
        assertSame(maya, createdMembers?.single())
    }
}
