package com.cruisemesh.app.debug

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.content.ContextCompat
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.chat.RealMeshSender
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.mesh.MeshService
import com.cruisemesh.app.mesh.RelaySyncEvents

private const val TAG = "DebugCommands"

/**
 * Debug-only adb hook for live device verification without UI scripting.
 * This receiver is merged only into debug builds via src/debug.
 */
class DebugCommandReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            ACTION_SEND_TEXT -> handleSendText(context, intent)
            ACTION_REQUEST_RELAY_SYNC -> handleRequestRelaySync()
            ACTION_START_MESH -> handleStartMesh(context)
            else -> Log.w(TAG, "Ignoring unknown debug action=${intent.action}")
        }
    }

    private fun handleSendText(context: Context, intent: Intent) {
        val identity = IdentityStore.load(context)
        if (identity == null) {
            Log.w(TAG, "SEND_TEXT ignored: no persisted identity")
            return
        }
        val contactUserIdHex = intent.getStringExtra(EXTRA_CONTACT_USER_ID_HEX)
        val text = intent.getStringExtra(EXTRA_TEXT)
        if (contactUserIdHex.isNullOrBlank() || text.isNullOrBlank()) {
            Log.w(TAG, "SEND_TEXT ignored: extras ${EXTRA_CONTACT_USER_ID_HEX} and ${EXTRA_TEXT} are required")
            return
        }

        val store = AppStore.get(context)
        val contact = try {
            store.getContact(UserIdHex.decode(contactUserIdHex))
        } catch (e: IllegalArgumentException) {
            Log.w(TAG, "SEND_TEXT ignored: invalid userId hex '$contactUserIdHex'")
            null
        }
        if (contact == null) {
            Log.w(TAG, "SEND_TEXT ignored: contact not found for $contactUserIdHex")
            return
        }

        RealMeshSender(store, identity).sendText(contact, text)
        Log.i(TAG, "SEND_TEXT queued for ${contact.name} (${UserIdHex.encode(contact.userId)}): '$text'")
    }

    private fun handleRequestRelaySync() {
        RelaySyncEvents.requestSync()
        Log.i(TAG, "REQUEST_RELAY_SYNC signaled")
    }

    private fun handleStartMesh(context: Context) {
        ContextCompat.startForegroundService(context, Intent(context, MeshService::class.java))
        Log.i(TAG, "START_MESH requested")
    }

    companion object {
        const val ACTION_SEND_TEXT = "com.cruisemesh.app.debug.SEND_TEXT"
        const val ACTION_REQUEST_RELAY_SYNC = "com.cruisemesh.app.debug.REQUEST_RELAY_SYNC"
        const val ACTION_START_MESH = "com.cruisemesh.app.debug.START_MESH"
        const val EXTRA_CONTACT_USER_ID_HEX = "contact_user_id_hex"
        const val EXTRA_TEXT = "text"
    }
}
