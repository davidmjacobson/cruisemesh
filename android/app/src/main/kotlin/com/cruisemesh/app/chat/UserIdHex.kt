package com.cruisemesh.app.chat

/**
 * Hex encode/decode for a [uniffi.cruisemesh_core.Contact.userId], used to
 * carry a UserID through the `"chat/{userIdHex}"` Compose Navigation route
 * argument -- nav route args are strings, and a UserID is raw bytes.
 */
object UserIdHex {
    private val HEX_CHARS = "0123456789abcdef".toCharArray()

    /** Lowercase hex, two characters per byte, e.g. `[0x0A, 0xFF]` -> `"0aff"`. */
    fun encode(bytes: ByteArray): String {
        val out = CharArray(bytes.size * 2)
        for (i in bytes.indices) {
            val b = bytes[i].toInt() and 0xFF
            out[i * 2] = HEX_CHARS[b ushr 4]
            out[i * 2 + 1] = HEX_CHARS[b and 0x0F]
        }
        return String(out)
    }

    /** Inverse of [encode]. Throws [IllegalArgumentException] on malformed input. */
    fun decode(hex: String): ByteArray {
        require(hex.length % 2 == 0) { "hex string must have even length: $hex" }
        return ByteArray(hex.length / 2) { i ->
            hex.substring(i * 2, i * 2 + 2).toInt(16).toByte()
        }
    }
}
