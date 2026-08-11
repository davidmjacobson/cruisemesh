package com.cruisemesh.app.chat

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.remember
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
import com.cruisemesh.app.mesh.ReachabilityLevel
import com.cruisemesh.app.ui.CruiseMeshTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.cruisemesh_core.Contact

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

    @Test
    fun keyboardDoesNotHideRecipientName() {
        var draft by mutableStateOf("")
        val bobId = byteArrayOf(0x01, 0x02)
        val bob = Contact(
            userId = bobId,
            name = "Bob",
            signPk = ByteArray(32),
            agreePk = ByteArray(32),
            relayUrl = null,
            relayToken = null,
        )

        compose.setContent {
            CruiseMeshTheme {
                val host = rememberConversationHost(bobId)
                ConversationScaffold(
                    host = host,
                    topBar = {
                        ConversationTopBar(
                            contact = bob,
                            displayId = "0102",
                            displayName = "Bob",
                            statusText = "Nearby",
                            reachability = ReachabilityLevel.NEARBY,
                            avatarBytes = null,
                            onBack = {},
                            onOpenDetails = {},
                        )
                    },
                    snackbarHostState = remember { SnackbarHostState() },
                    listContent = {
                        items((1..20).toList()) { Text("Message $it") }
                    },
                    belowList = {
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
                    },
                )
            }
        }

        compose.onNodeWithContentDescription("Contact details for Bob").assertIsDisplayed()
        compose.onNode(hasSetTextAction()).performClick().performTextInput("Still Bob")
        compose.waitForIdle()
        compose.onNodeWithContentDescription("Contact details for Bob").assertIsDisplayed()
    }
}
