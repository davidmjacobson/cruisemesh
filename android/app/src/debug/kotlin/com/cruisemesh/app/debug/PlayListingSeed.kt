package com.cruisemesh.app.debug

import android.content.Context
import android.util.Log
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.identity.OnboardingStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.identity.TermsAcceptanceStore
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.coreLegacyDeviceId
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.generateIdentity

private const val TAG = "PlayListingSeed"
private const val PREFS = "cruisemesh_play_listing_seed"
private const val PREF_VERSION = "version"
private const val SEED_VERSION = 2

private const val KIND_TEXT: UByte = 1u
private const val RECEIPT_DELIVERED: UByte = 1u
private const val RECEIPT_READ: UByte = 2u

/** Display name the listing shots are taken as. */
const val PLAY_LISTING_HERO_NAME = "Simmy"

/**
 * Debug-only inbox used for Play listing screenshots. Inserts a small family
 * of real identities and plaintext chat rows so the home list and a couple of
 * open threads look lived-in. Not a protocol fixture and not a substitute
 * for two-phone delivery tests.
 */
object PlayListingSeed {
    fun apply(context: Context): Boolean {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (prefs.getInt(PREF_VERSION, 0) >= SEED_VERSION) {
            Log.i(TAG, "already applied (v$SEED_VERSION)")
            return false
        }

        TermsAcceptanceStore.acceptCurrentVersion(context)
        OnboardingStore.markCompleted(context)
        if (ProfileStore.loadStoredDisplayName(context).isEmpty()) {
            ProfileStore.saveDisplayName(context, PLAY_LISTING_HERO_NAME)
        }

        val identity = IdentityStore.load(context)
            ?: generateIdentity().also { IdentityStore.save(context, it) }
        val store = AppStore.get(context)
        val now = System.currentTimeMillis()
        val min = 60_000L

        val maya = person("Maya")
        val sam = person("Sam")
        val jordan = person("Jordan")
        store.upsertContact(maya.contact)
        store.upsertContact(sam.contact)
        store.upsertContact(jordan.contact)

        // Each sender keeps its own contiguous lamport stream. A shared
        // counter opens visible-gap banners in the thread.
        put(store, maya.id, maya.id, 1uL, now - 45 * min, "On the lido deck. Coffee?")
        put(store, maya.id, identity.userId, 1uL, now - 42 * min, "On my way")
        put(store, maya.id, maya.id, 2uL, now - 8 * min, "Grabbed a table by the windows")
        put(store, maya.id, identity.userId, 2uL, now - 6 * min, "Be there in 5")
        markReadByUs(store, maya.id, maya.id, 2uL)
        markReadByThem(store, identity.userId, maya.id, 2uL)

        put(store, sam.id, identity.userId, 1uL, now - 3 * 60 * min, "Show's at 8 — want to go?")
        put(store, sam.id, sam.id, 1uL, now - 12 * min, "Yes!! Can we sit near the front?")
        put(store, sam.id, sam.id, 2uL, now - 11 * min, "Mom said she'll come too")
        markDeliveredByThem(store, identity.userId, sam.id, 1uL)

        put(store, jordan.id, jordan.id, 1uL, now - 26 * 60 * min, "Pool's packed. Trying deck 12.")
        put(store, jordan.id, identity.userId, 1uL, now - 25 * 60 * min, "Found chairs by the slide")
        markReadByUs(store, jordan.id, jordan.id, 1uL)
        markReadByThem(store, identity.userId, jordan.id, 1uL)

        val dinner = createGroup(
            "Dinner",
            listOf(identity.userId, maya.id, sam.id, jordan.id),
        )
        store.upsertGroup(dinner)
        put(store, dinner.id, maya.id, 1uL, now - 25 * min, "Italian or the buffet tonight?")
        put(store, dinner.id, sam.id, 1uL, now - 24 * min, "Italian!!")
        put(store, dinner.id, identity.userId, 1uL, now - 20 * min, "Italian. 7 work?")
        put(store, dinner.id, jordan.id, 1uL, now - 2 * min, "See you at the atrium")
        markReadByUs(store, dinner.id, maya.id, 1uL)
        markReadByUs(store, dinner.id, sam.id, 1uL)

        prefs.edit().putInt(PREF_VERSION, SEED_VERSION).apply()
        Log.i(TAG, "seeded Maya, Sam, Jordan, and Dinner")
        return true
    }

    private class Person(val identity: Identity, val name: String) {
        val id: ByteArray get() = identity.userId
        val contact: Contact
            get() = Contact(
                userId = identity.userId,
                name = name,
                signPk = identity.signPk,
                agreePk = identity.agreePk,
                relayUrl = null,
                relayToken = null,
            )
    }

    private fun person(name: String) = Person(generateIdentity(), name)

    private fun put(
        store: MessageStore,
        chatId: ByteArray,
        sender: ByteArray,
        lamport: ULong,
        timestamp: Long,
        text: String,
    ) {
        store.insertMessage(
            StoredMessage(
                chatId = chatId,
                senderUserId = sender,
                lamport = lamport,
                timestamp = timestamp,
                kind = KIND_TEXT,
                payload = text.toByteArray(Charsets.UTF_8),
                senderDeviceId = coreLegacyDeviceId(),
            ),
        )
    }

    private fun markReadByUs(
        store: MessageStore,
        chatId: ByteArray,
        sender: ByteArray,
        through: ULong,
    ) {
        store.recordOutgoingReceipt(chatId, sender, RECEIPT_DELIVERED, through)
        store.recordOutgoingReceipt(chatId, sender, RECEIPT_READ, through)
    }

    private fun markDeliveredByThem(
        store: MessageStore,
        us: ByteArray,
        them: ByteArray,
        through: ULong,
    ) {
        store.recordReceipt(them, us, RECEIPT_DELIVERED, through, null, null)
    }

    private fun markReadByThem(
        store: MessageStore,
        us: ByteArray,
        them: ByteArray,
        through: ULong,
    ) {
        store.recordReceipt(them, us, RECEIPT_DELIVERED, through, null, null)
        store.recordReceipt(them, us, RECEIPT_READ, through, null, null)
    }
}
