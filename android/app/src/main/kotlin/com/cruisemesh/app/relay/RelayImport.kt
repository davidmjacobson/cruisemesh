package com.cruisemesh.app.relay

import android.content.Context
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.relayTokenIsDeposit

/** Native fallback persistence around the canonical Rust import upsert. */
object RelayImport {
    fun reconcileOnImport(context: Context, store: MessageStore, incoming: Contact): Contact {
        val relayUrl = incoming.relayUrl?.trim()
        val relayToken = incoming.relayToken?.trim()
        // CP4: post-CP4 friend cards carry a post-only deposit token. That
        // is a fine credential for the contact record (sends resolve through
        // it), but never for this phone's OWN config — adopting it would
        // leave the phone unable to fetch its own mail (403 deposit_only on
        // every poll). Own config comes from the Shore Pass setup card,
        // which stays member-scoped.
        if (RelayConfigStore.load(context) == null &&
            !relayUrl.isNullOrEmpty() &&
            !relayToken.isNullOrEmpty() &&
            !relayTokenIsDeposit(relayToken)
        ) {
            RelayConfigStore.save(context, relayUrl, relayToken)
        }
        return store.upsertImportedContact(incoming)
    }
}
