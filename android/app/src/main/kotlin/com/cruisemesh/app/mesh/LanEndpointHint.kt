package com.cruisemesh.app.mesh

import java.nio.ByteBuffer

internal const val LAN_ENDPOINT_HINT_FRAME_TYPE: Byte = 0x04
private const val LAN_ENDPOINT_HINT_VERSION: Byte = 1
private const val INSTANCE_TOKEN_LENGTH = 16
private const val MAX_HOST_BYTES = 255

internal data class LanEndpointHint(
    val instanceToken: String,
    val endpoint: LanManualEndpoint,
)

/**
 * Link-local control frame introducing an accepted BLE peer to this phone's
 * current Wi-Fi listener. The resulting TCP socket must still pass Noise
 * authentication against the accepted contact key.
 */
internal fun encodeLanEndpointHint(hint: LanEndpointHint): ByteArray {
    require(
        hint.instanceToken.length == INSTANCE_TOKEN_LENGTH &&
            hint.instanceToken.all { it.isDigit() || it in 'a'..'f' },
    )
    val host = hint.endpoint.host.toByteArray(Charsets.UTF_8)
    require(host.isNotEmpty() && host.size <= MAX_HOST_BYTES)
    return ByteBuffer.allocate(1 + 1 + INSTANCE_TOKEN_LENGTH + 2 + 1 + host.size)
        .put(LAN_ENDPOINT_HINT_FRAME_TYPE)
        .put(LAN_ENDPOINT_HINT_VERSION)
        .put(hint.instanceToken.toByteArray(Charsets.US_ASCII))
        .putShort(hint.endpoint.port.toShort())
        .put(host.size.toByte())
        .put(host)
        .array()
}

internal fun decodeLanEndpointHint(frame: ByteArray): LanEndpointHint? {
    if (frame.size < 1 + 1 + INSTANCE_TOKEN_LENGTH + 2 + 1) return null
    val buffer = ByteBuffer.wrap(frame)
    if (buffer.get() != LAN_ENDPOINT_HINT_FRAME_TYPE) return null
    if (buffer.get() != LAN_ENDPOINT_HINT_VERSION) return null
    val tokenBytes = ByteArray(INSTANCE_TOKEN_LENGTH).also(buffer::get)
    val token = tokenBytes.toString(Charsets.US_ASCII)
    if (token.any { !it.isDigit() && it !in 'a'..'f' }) return null
    val port = buffer.short.toInt() and 0xffff
    val hostLength = buffer.get().toInt() and 0xff
    if (port == 0 || hostLength == 0 || buffer.remaining() != hostLength) return null
    val host = ByteArray(hostLength).also(buffer::get).toString(Charsets.UTF_8)
    if (host.isBlank() || host.any(Char::isWhitespace)) return null
    return LanEndpointHint(token, LanManualEndpoint(host, port))
}
