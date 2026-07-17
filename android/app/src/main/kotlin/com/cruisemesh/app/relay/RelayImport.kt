package com.cruisemesh.app.relay

import android.content.Context
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageStore

/** Native fallback persistence around the canonical Rust import upsert. */
object RelayImport {
    fun reconcileOnImport(context: Context, store: MessageStore, incoming: Contact): Contact {
        val relayUrl = incoming.relayUrl?.trim()
        val relayToken = incoming.relayToken?.trim()
        if (RelayConfigStore.load(context) == null && !relayUrl.isNullOrEmpty() && !relayToken.isNullOrEmpty()) {
            RelayConfigStore.save(context, relayUrl, relayToken)
        }
        return store.upsertImportedContact(incoming)
    }
}
