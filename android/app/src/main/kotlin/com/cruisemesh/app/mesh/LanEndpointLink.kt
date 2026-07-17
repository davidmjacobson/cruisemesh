package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreLanEndpoint
import uniffi.cruisemesh_core.coreMakeLanEndpointLink
import uniffi.cruisemesh_core.coreParseLanEndpointLink

internal fun lanEndpointLink(endpoint: LanManualEndpoint): String {
    return coreMakeLanEndpointLink(CoreLanEndpoint(endpoint.host, endpoint.port.toUShort()))
}

internal fun parseLanEndpointLink(fragment: String?): LanManualEndpoint? {
    val endpoint = coreParseLanEndpointLink(fragment) ?: return null
    return LanManualEndpoint(endpoint.host, endpoint.port.toInt())
}
