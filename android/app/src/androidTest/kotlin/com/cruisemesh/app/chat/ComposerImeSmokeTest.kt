package com.cruisemesh.app.chat

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Surface
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.ui.CruiseMeshTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ComposerImeSmokeTest {
    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun keyboardDoesNotHideSendAction() {
        var draft by mutableStateOf("")
        compose.setContent {
            CruiseMeshTheme {
                Surface(modifier = Modifier.fillMaxSize().padding(horizontal = 12.dp)) {
                    MessageComposer(
                        draft = draft,
                        onDraftChange = { draft = it },
                        onSend = {},
                        hasPendingAttachment = false,
                        ownBubbleColor = Color(0xFF236A5B),
                        onPickGallery = {},
                        onPickCamera = {},
                        onStartVoice = { true },
                        onStopVoice = {},
                        onCancelVoice = {},
                    )
                }
            }
        }

        compose.onNode(hasSetTextAction()).performClick().performTextInput("Still visible")
        compose.waitForIdle()
        compose.onNodeWithContentDescription("Send").assertIsDisplayed()
    }
}
