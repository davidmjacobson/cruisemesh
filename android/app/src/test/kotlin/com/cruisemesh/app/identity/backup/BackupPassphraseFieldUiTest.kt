package com.cruisemesh.app.identity.backup

import android.content.Context
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.autofill.AutofillTree
import androidx.compose.ui.autofill.AutofillType
import androidx.compose.ui.platform.LocalAutofillTree
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.R
import com.cruisemesh.app.ui.CruiseMeshTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@OptIn(ExperimentalComposeUiApi::class)
@RunWith(AndroidJUnit4::class)
class BackupPassphraseFieldUiTest {
    @get:Rule
    val compose = createComposeRule()

    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test
    fun creationFieldRegistersAsNewPasswordAndCanBeRevealed() {
        val tree = AutofillTree()
        var value by mutableStateOf("")

        compose.setContent {
            CompositionLocalProvider(LocalAutofillTree provides tree) {
                CruiseMeshTheme {
                    PassphraseField(
                        value = value,
                        onValueChange = { value = it },
                        label = "Backup passphrase",
                        autofillType = AutofillType.NewPassword,
                    )
                }
            }
        }

        compose.onNodeWithText("Backup passphrase").performTextInput("correct horse")
        compose.onNodeWithText(context.getString(R.string.ui_show_passphrase)).performClick()
        compose.onNodeWithText("correct horse").assertIsDisplayed()
        compose.onNodeWithText(context.getString(R.string.ui_hide_passphrase)).assertIsDisplayed()

        assertEquals(listOf(AutofillType.NewPassword), tree.children.values.single().autofillTypes)
    }

    @Test
    fun restoreFieldRegistersAsExistingPassword() {
        val tree = AutofillTree()

        compose.setContent {
            CompositionLocalProvider(LocalAutofillTree provides tree) {
                CruiseMeshTheme {
                    PassphraseField(
                        value = "",
                        onValueChange = {},
                        label = "Backup passphrase",
                        autofillType = AutofillType.Password,
                    )
                }
            }
        }

        compose.waitForIdle()
        assertEquals(listOf(AutofillType.Password), tree.children.values.single().autofillTypes)
    }

    @Test
    fun savedDialogMakesCompletionExplicit() {
        var dismissed = false
        compose.setContent {
            CruiseMeshTheme {
                BackupSavedDialog(onDismiss = { dismissed = true })
            }
        }

        compose.onNodeWithText(context.getString(R.string.ui_backup_saved)).assertIsDisplayed()
        compose.onNodeWithText(
            context.getString(R.string.ui_backup_saved_keep_it_and_your_passphrase),
        ).assertIsDisplayed()
        compose.onNodeWithText(context.getString(R.string.ui_done)).performClick()

        assertEquals(true, dismissed)
    }
}
