package com.cruisemesh.app.identity

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
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.DeviceKeypair
import uniffi.cruisemesh_core.coreDecodeDeviceKeypair
import uniffi.cruisemesh_core.coreEncodeDeviceKeypair
import uniffi.cruisemesh_core.generateDeviceKeypair

private const val PREFS_NAME = "cruisemesh_device_key"
private const val PREF_CIPHERTEXT = "device_key_ciphertext"
private const val PREF_IV = "device_key_iv"
private const val KEYSTORE_ALIAS = "cruisemesh_device_key"
private const val ANDROID_KEYSTORE = "AndroidKeyStore"
private const val TRANSFORMATION = "AES/GCM/NoPadding"
private const val GCM_TAG_LENGTH_BITS = 128
private const val TAG = "DeviceKeyStore"

/**
 * This install's own device keys (`specs/multi-device-v1.md` §3), stored the
 * way [IdentityStore] stores the person's: AES-256-GCM under a key that lives
 * in the Android Keystore and never leaves it.
 *
 * Two keys, one purpose. §3 splits the deployed Ed25519 identity into a *person
 * root* -- whose secret, after migration, exists only inside the passphrase-
 * encrypted `.cmbak` (§14.2) -- and a *device* signing key that this phone
 * holds and uses for roster signatures, the §9.3 device offer, and the §9.4
 * activation acknowledgement. The person root is not here, deliberately: a
 * thief holding this phone must not be able to revoke the person's real
 * devices.
 *
 * The blob layout is the core's ([coreEncodeDeviceKeypair]) so that iOS and the
 * desktop store the identical bytes rather than each inventing a layout.
 */
object DeviceKeyStore {

    /**
     * The keys this device signs with, minted on first use.
     *
     * Minting is idempotent per install and never per ceremony: a device that
     * re-keyed itself between the offer and the acknowledgement would hold a
     * certificate for a key it had already thrown away (DL-4 -- re-linking the
     * same hardware is what mints a fresh key, and that is a deliberate act).
     */
    fun loadOrCreate(context: Context): DeviceKeypair =
        load(context) ?: generateDeviceKeypair().also { save(context, it) }

    /** The stored keys, or null on an install that has never needed any. */
    fun load(context: Context): DeviceKeypair? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val ciphertext = prefs.getString(PREF_CIPHERTEXT, null)?.let { decodeBase64(it) } ?: return null
        val iv = prefs.getString(PREF_IV, null)?.let { decodeBase64(it) } ?: return null

        return try {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, getOrCreateKey(), GCMParameterSpec(GCM_TAG_LENGTH_BITS, iv))
            coreDecodeDeviceKeypair(cipher.doFinal(ciphertext))
        } catch (e: GeneralSecurityException) {
            // Same call as IdentityStore's, and the same reasoning: nothing can
            // recover keys the Keystore can no longer unwrap, so drop the stale
            // blob rather than fail every launch. A device that loses its keys
            // has to be linked again, which mints fresh ones (DL-4).
            Log.w(TAG, "Discarding undecryptable stored device keys", e)
            clear(context)
            null
        } catch (e: CoreException) {
            Log.w(TAG, "Discarding corrupt stored device keys", e)
            clear(context)
            null
        }
    }

    private fun save(context: Context, device: DeviceKeypair) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, getOrCreateKey())
        val ciphertext = cipher.doFinal(coreEncodeDeviceKeypair(device))

        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
            .putString(PREF_CIPHERTEXT, encodeBase64(ciphertext))
            .putString(PREF_IV, encodeBase64(cipher.iv))
            .apply()
    }

    private fun clear(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
            .remove(PREF_CIPHERTEXT)
            .remove(PREF_IV)
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
