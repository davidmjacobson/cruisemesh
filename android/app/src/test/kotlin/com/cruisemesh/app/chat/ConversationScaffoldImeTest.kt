package com.cruisemesh.app.chat

import android.view.View
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.dp
import androidx.core.graphics.Insets
import androidx.core.view.WindowInsetsCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.ui.CruiseMeshTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The app is edge-to-edge, so the manifest's adjustResize declaration stops
 * the window from panning but resizes nothing: the keyboard arrives as an
 * IME inset the layout must consume itself, and for a while the chat
 * Scaffold didn't -- in the field the header stayed pinned while the
 * keyboard slid straight over the composer. [ConversationScaffold] now
 * unions the IME into its content insets; this pins the observable outcome
 * by dispatching a synthetic IME inset to the Compose host view, exactly as
 * the window would deliver a real keyboard.
 */
@RunWith(AndroidJUnit4::class)
class ConversationScaffoldImeTest {

    @get:Rule
    val compose = createComposeRule()

    private var hostView: View? = null

    private fun setScaffoldContent() {
        compose.setContent {
            hostView = LocalView.current
            CruiseMeshTheme {
                val host = rememberConversationHost(chatId = byteArrayOf(1))
                ConversationScaffold(
                    host = host,
                    topBar = { Text("Maya", modifier = Modifier.testTag("topbar")) },
                    snackbarHostState = remember { SnackbarHostState() },
                    listContent = {},
                    belowList = {
                        Box(
                            modifier = Modifier
                                .testTag("composer")
                                .fillMaxWidth()
                                .height(48.dp),
                        )
                    },
                )
            }
        }
    }

    /** Delivers a bottom IME inset to the Compose host, as the window would. */
    private fun dispatchBottomIme(imePx: Int) {
        val insets = WindowInsetsCompat.Builder()
            .setInsets(WindowInsetsCompat.Type.ime(), Insets.of(0, 0, 0, imePx))
            .setVisible(WindowInsetsCompat.Type.ime(), imePx > 0)
            .build()
            .toWindowInsets()!!
        compose.runOnUiThread {
            checkNotNull(hostView).dispatchApplyWindowInsets(insets)
        }
        compose.waitForIdle()
    }

    @Test
    fun `composer rises above the keyboard and the top bar stays pinned`() {
        setScaffoldContent()
        compose.waitForIdle()

        val rootBottom = compose.onRoot().getUnclippedBoundsInRoot().bottom
        val restingBottom = compose.onNodeWithTag("composer").getUnclippedBoundsInRoot().bottom
        val pinnedTop = compose.onNodeWithTag("topbar").getUnclippedBoundsInRoot().top

        dispatchBottomIme(imePx = 300)

        // Robolectric's default density is mdpi, so 300 px == 300.dp.
        val raisedBottom = compose.onNodeWithTag("composer").getUnclippedBoundsInRoot().bottom
        assertTrue(
            "composer bottom sits at $raisedBottom; with a 300px keyboard up it must" +
                " clear $rootBottom - 300dp or the input box is buried under the IME",
            raisedBottom <= rootBottom - 300.dp,
        )
        assertEquals(
            "the top bar must not move when the keyboard opens",
            pinnedTop,
            compose.onNodeWithTag("topbar").getUnclippedBoundsInRoot().top,
        )

        dispatchBottomIme(imePx = 0)

        assertEquals(
            "the composer must settle back to its resting position when the keyboard hides",
            restingBottom,
            compose.onNodeWithTag("composer").getUnclippedBoundsInRoot().bottom,
        )
    }
}
