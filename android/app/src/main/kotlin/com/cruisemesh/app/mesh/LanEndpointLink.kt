package com.cruisemesh.app.mesh

import java.util.Base64

private const val LAN_LINK_PREFIX = "CMLAN1:"

internal fun lanEndpointLink(endpoint: LanManualEndpoint): String {
    val host = Base64.getUrlEncoder().withoutPadding()
        .encodeToString(endpoint.host.toByteArray(Charsets.UTF_8))
    return "https://cruisemesh.app/lan#$LAN_LINK_PREFIX$host:${endpoint.port}"
}

internal fun parseLanEndpointLink(fragment: String?): LanManualEndpoint? {
    val payload = fragment?.takeIf { it.startsWith(LAN_LINK_PREFIX) }
        ?.removePrefix(LAN_LINK_PREFIX)
        ?: return null
    val separator = payload.lastIndexOf(':')
    if (separator <= 0 || separator == payload.lastIndex) return null
    val host = try {
        Base64.getUrlDecoder().decode(payload.substring(0, separator))
            .toString(Charsets.UTF_8)
    } catch (_: IllegalArgumentException) {
        return null
    }
    val port = payload.substring(separator + 1).toIntOrNull() ?: return null
    if (host.isBlank() || host.any(Char::isWhitespace) || port !in 1..65_535) return null
    return LanManualEndpoint(host, port)
}
