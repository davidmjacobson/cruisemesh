package com.cruisemesh.app.chat

import android.graphics.Bitmap
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.ui.BubbleGrouping
import com.cruisemesh.app.ui.CruiseMeshTheme
import java.io.ByteArrayOutputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.cruisemesh_core.StoredMessage

@RunWith(AndroidJUnit4::class)
class GroupPhotoViewerUiTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun tappingGroupPhotoDispatchesViewerBytes() {
        val jpeg = ByteArrayOutputStream().use { output ->
            Bitmap.createBitmap(2, 2, Bitmap.Config.ARGB_8888)
                .compress(Bitmap.CompressFormat.JPEG, 90, output)
            output.toByteArray()
        }
        val message = StoredMessage(
            chatId = byteArrayOf(1),
            senderUserId = byteArrayOf(2),
            lamport = 1uL,
            timestamp = 1L,
            kind = KIND_ATTACHMENT_MANIFEST,
            payload = AttachmentPayload(
                mediaType = AttachmentPayload.MediaType.IMAGE,
                mimeType = "image/jpeg",
                durationMs = 0,
                blob = jpeg,
            ).encode(),
        )
        var opened: ByteArray? = null

        compose.setContent {
            CruiseMeshTheme {
                GroupMessageBubble(
                    message = message,
                    tick = null,
                    isFocused = false,
                    isOwn = false,
                    senderLabel = "Alice",
                    groupName = "Family",
                    grouping = BubbleGrouping(joinsPrevious = false, joinsNext = false),
                    onPhotoClick = { opened = it },
                )
            }
        }

        compose.waitForIdle()
        compose.onNodeWithContentDescription("Photo — tap to view full screen").performClick()

        assertArrayEquals(jpeg, opened)
    }
}
