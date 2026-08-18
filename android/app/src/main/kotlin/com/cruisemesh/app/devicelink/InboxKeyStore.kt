package com.cruisemesh.app.devicelink

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import android.util.Log
import java.security.GeneralSecurityException
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.InboxKey

private const val PREFS_NAME = "cruisemesh_inbox_key"
private const val PREF_GENERATION = "generation"
private const val PREF_AGREE_PK = "agree_pk"
private const val PREF_SECRET_CIPHERTEXT = "agree_sk_ciphertext"
private const val PREF_SECRET_IV = "agree_sk_iv"
private const val KEYSTORE_ALIAS = "cruisemesh_inbox_key"
private const val ANDROID_KEYSTORE = "AndroidKeyStore"
private const val TRANSFORMATION = "AES/GCM/NoPadding"
private const val GCM_TAG_LENGTH_BITS = 128
private const val TAG = "InboxKeyStore"

/**
 * §6's person-scoped inbox key, at whatever generation this fleet has reached.
 *
 * The core generates it and never persists it
 * ([uniffi.cruisemesh_core.InboxKey]'s own contract), so somebody has to, and
 * §10.1's two-call ceremony makes *when* load-bearing: `beginOwnRevocation`
 * hands the rotated key out, this file has to make it durable, and only then
 * may `commitOwnRevocation` re-seal the backlog to it. A crash between those
 * two is recoverable exactly because [generation] can be asked afterwards
 * whether the key survived.
 *
 * # Generation 0 is not stored, because it is not a secret of its own
 *
 * §10's note 4: "Inbox key generation 0 *is* the deployed person agreement
 * key". Every install in the field already holds it as
 * [Identity.agreeSk] — it is the key on every friend card — so writing a second
 * copy here would be a second place for the same secret to leak from and a
 * second place for a restore to disagree with itself. [current] therefore
 * *derives* generation 0 from the identity and only reads storage above it.
 *
 * # Layout
 *
 * Three fields, and no invented blob format: the generation and the public half
 * are plaintext preferences because both are public (the generation rides in
 * every roster; the public key is what siblings seal to), and the ciphertext is
 * exactly the 32-byte secret under AES-256-GCM with a key that lives in the
 * Android Keystore — the same shape [com.cruisemesh.app.identity.DeviceKeyStore]
 * uses. Storing the secret alone rather than an encoded record is what keeps
 * this from being a wire format that iOS would have to match byte for byte.
 */
object InboxKeyStore {

    /**
     * The key for `generation`, or null if this device does not hold it.
     *
     * Generation 0 is answered from `identity` (see the class note); every
     * other generation must have been written down by [save].
     */
    fun keyFor(context: Context, identity: Identity, generation: ULong): InboxKey? {
        if (generation == 0uL) {
            return InboxKey(
                generation = 0uL,
                agreePk = identity.agreePk,
                agreeSk = identity.agreeSk,
            )
        }
        val stored = load(context) ?: return null
        return stored.takeIf { it.generation == generation }
    }

    /**
     * The key this fleet's roster currently names, for a caller that is about
     * to rotate it. Null means the roster has climbed to a generation whose key
     * this device never received — a sibling that has not caught up, and a
     * revocation it must not attempt.
     */
    fun current(context: Context, identity: Identity, inboxKeyGeneration: ULong): InboxKey? =
        keyFor(context, identity, inboxKeyGeneration)

    /** The stored key, or null on an install that has never rotated one. */
    fun load(context: Context): InboxKey? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val generation = prefs.getLong(PREF_GENERATION, -1L).takeIf { it >= 0L } ?: return null
        val agreePk = prefs.getString(PREF_AGREE_PK, null)?.let { decodeBase64(it) } ?: return null
        val ciphertext = prefs.getString(PREF_SECRET_CIPHERTEXT, null)?.let { decodeBase64(it) } ?: return null
        val iv = prefs.getString(PREF_SECRET_IV, null)?.let { decodeBase64(it) } ?: return null
        return try {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, getOrCreateKey(), GCMParameterSpec(GCM_TAG_LENGTH_BITS, iv))
            InboxKey(generation.toULong(), agreePk, cipher.doFinal(ciphertext))
        } catch (e: GeneralSecurityException) {
            // Unlike a device key, this one must NOT be silently dropped and
            // re-minted: rows are sealed to it, and a fresh key would not open
            // them. Leave the blob exactly where it is and answer honestly that
            // this device cannot read it, so the caller declines the rotation
            // instead of performing one it cannot finish.
            Log.w(TAG, "stored inbox key could not be decrypted", e)
            null
        }
    }

    /** Which generation this device holds, for §10.1's crash-recovery question. */
    fun generation(context: Context): ULong? =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getLong(PREF_GENERATION, -1L)
            .takeIf { it >= 0L }
            ?.toULong()

    /**
     * Make a rotated key durable. Must return before
     * `MessageStore.commitOwnRevocation` runs — the whole point of the two-call
     * ceremony is that the backlog is never re-sealed to a secret that only
     * exists in memory.
     */
    fun save(context: Context, key: InboxKey) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, getOrCreateKey())
        val ciphertext = cipher.doFinal(key.agreeSk)
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
            .putLong(PREF_GENERATION, key.generation.toLong())
            .putString(PREF_AGREE_PK, encodeBase64(key.agreePk))
            .putString(PREF_SECRET_CIPHERTEXT, encodeBase64(ciphertext))
            .putString(PREF_SECRET_IV, encodeBase64(cipher.iv))
            .apply()
    }

    private fun getOrCreateKey(): SecretKey {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (keyStore.getKey(KEYSTORE_ALIAS, null) as? SecretKey)?.let { return it }
        val keyGenerator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        keyGenerator.init(
            KeyGenParameterSpec.Builder(
                KEYSTORE_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return keyGenerator.generateKey()
    }

    private fun decodeBase64(value: String): ByteArray = Base64.decode(value, Base64.NO_WRAP)
    private fun encodeBase64(value: ByteArray): String = Base64.encodeToString(value, Base64.NO_WRAP)
}
