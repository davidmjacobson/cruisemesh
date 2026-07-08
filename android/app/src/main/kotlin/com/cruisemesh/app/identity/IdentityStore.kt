package com.cruisemesh.app.identity

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import uniffi.cruisemesh_core.Identity

private const val PREFS_NAME = "cruisemesh_identity"
private const val PREF_CIPHERTEXT = "identity_ciphertext"
private const val PREF_IV = "identity_iv"
private const val KEYSTORE_ALIAS = "cruisemesh_identity_key"
private const val ANDROID_KEYSTORE = "AndroidKeyStore"
private const val TRANSFORMATION = "AES/GCM/NoPadding"
private const val GCM_TAG_LENGTH_BITS = 128

/** Fixed field sizes in [Identity]: userId, signPk, signSk, agreePk, agreeSk. */
private val IDENTITY_FIELD_SIZES = listOf(16, 32, 32, 32, 32)

/**
 * Persists the on-device [Identity] (Ed25519 + X25519 secret key material)
 * across app restarts, encrypted with an AES-256-GCM key that lives in the
 * Android Keystore and never leaves it. `core/src/identity.rs` deliberately
 * doesn't persist anything itself -- its doc comment calls out Android
 * Keystore-backed storage as the app's responsibility (DESIGN.md §6.2).
 * Field sizes are all fixed, so [encodeIdentity]/[decodeIdentity] use plain
 * concatenation rather than a length-prefixed format.
 */
object IdentityStore {

    fun load(context: Context): Identity? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val ciphertext = prefs.getString(PREF_CIPHERTEXT, null)?.let { decodeBase64(it) } ?: return null
        val iv = prefs.getString(PREF_IV, null)?.let { decodeBase64(it) } ?: return null

        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, getOrCreateKey(), GCMParameterSpec(GCM_TAG_LENGTH_BITS, iv))
        return decodeIdentity(cipher.doFinal(ciphertext))
    }

    fun save(context: Context, identity: Identity) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, getOrCreateKey())
        val ciphertext = cipher.doFinal(encodeIdentity(identity))

        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
            .putString(PREF_CIPHERTEXT, encodeBase64(ciphertext))
            .putString(PREF_IV, encodeBase64(cipher.iv))
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

internal fun encodeIdentity(identity: Identity): ByteArray =
    identity.userId + identity.signPk + identity.signSk + identity.agreePk + identity.agreeSk

internal fun decodeIdentity(bytes: ByteArray): Identity {
    require(bytes.size == IDENTITY_FIELD_SIZES.sum()) {
        "corrupt stored identity: expected ${IDENTITY_FIELD_SIZES.sum()} bytes, got ${bytes.size}"
    }
    var offset = 0
    val fields = IDENTITY_FIELD_SIZES.map { size ->
        bytes.copyOfRange(offset, offset + size).also { offset += size }
    }
    return Identity(
        userId = fields[0],
        signPk = fields[1],
        signSk = fields[2],
        agreePk = fields[3],
        agreeSk = fields[4],
    )
}
