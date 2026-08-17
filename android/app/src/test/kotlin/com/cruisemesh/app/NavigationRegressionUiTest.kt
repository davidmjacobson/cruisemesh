package com.cruisemesh.app

import android.content.Intent
import androidx.activity.ComponentActivity
import androidx.navigation.compose.ComposeNavigator
import androidx.navigation.compose.composable
import androidx.navigation.createGraph
import androidx.navigation.testing.TestNavHostController
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.notify.MessageNotifier
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric

@RunWith(AndroidJUnit4::class)
class NavigationRegressionUiTest {
    @Test
    fun notificationRouteIsConsumedFromIntent() {
        val intent = Intent()
            .putExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX, "aabbccdd")
            .putExtra(MessageNotifier.EXTRA_CHAT_IS_GROUP, true)

        val link = consumePendingDeepLink(intent)

        assertEquals("aabbccdd", link?.idHex)
        assertTrue(link?.isGroup == true)
        assertFalse(intent.hasExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX))
        assertFalse(intent.hasExtra(MessageNotifier.EXTRA_CHAT_IS_GROUP))
        assertNull(consumePendingDeepLink(intent))
    }

    @Test
    fun emptyFriendFragmentIsNotAPendingDeepLink() {
        val intent = Intent(
            Intent.ACTION_VIEW,
            android.net.Uri.parse("https://cruisemesh.app/f#"),
        )
        assertNull(derivePendingDeepLink(intent))
    }

    /**
     * A device-link offer is scanned inside the linking ceremony, by the device
     * that is already part of this person. The route resolves, and it
     * deliberately opens nothing: a cold tap must not land someone in a flow
     * that cannot finish what the link starts.
     */
    @Test
    fun aDeviceLinkOfferOpensNoScreenOnItsOwn() {
        val intent = Intent(
            Intent.ACTION_VIEW,
            android.net.Uri.parse("https://cruisemesh.app/link#CMLINK1:AZoB"),
        )
        assertNull(derivePendingDeepLink(intent))
    }

    @Test
    fun backFromOnlyDestinationFinishesInsteadOfBlankingActivity() {
        val activity = Robolectric.buildActivity(ComponentActivity::class.java).setup().get()
        val nav = controller(activity)

        nav.popOrExit(activity)

        assertTrue(activity.isFinishing)
        assertEquals("home", nav.currentDestination?.route)
    }

    @Test
    fun backWithHistoryPopsToPreviousDestination() {
        val activity = Robolectric.buildActivity(ComponentActivity::class.java).setup().get()
        val nav = controller(activity)
        nav.navigate("settings")

        nav.popOrExit(activity)

        assertFalse(activity.isFinishing)
        assertEquals("home", nav.currentDestination?.route)
    }

    private fun controller(activity: ComponentActivity) = TestNavHostController(activity).apply {
        navigatorProvider.addNavigator(ComposeNavigator())
        graph = createGraph(startDestination = "home") {
            composable("home") {}
            composable("settings") {}
        }
    }
}
