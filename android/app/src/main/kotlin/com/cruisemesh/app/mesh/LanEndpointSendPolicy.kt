package com.cruisemesh.app.mesh

import java.util.Base64
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.shouldResendLanEndpoint

internal data class AuthenticatedLanEndpointHint(
    val contact: Contact,
    val hint: Frame.LanEndpoint,
    val networkId: String,
)

/** Selects the self-advertised hint for an authenticated contact, or skips incomplete/non-contact state. */
internal fun authenticatedLanEndpointHint(
    contact: Contact?,
    hint: Frame.LanEndpoint?,
    networkId: String?,
): AuthenticatedLanEndpointHint? {
    if (contact == null || hint == null || networkId == null) return null
    return AuthenticatedLanEndpointHint(contact, hint, networkId)
}

internal fun lanEndpointSignature(
    networkId: String,
    host: String,
    port: Int,
    instanceToken: ByteArray,
): String = listOf(
    networkId,
    host,
    port.toString(),
    Base64.getUrlEncoder().withoutPadding().encodeToString(instanceToken),
).joinToString("|")

internal fun shouldClaimLanEndpointSend(
    previousRecord: String?,
    currentSignature: String,
    nowMs: Long,
): Boolean {
    val previous = previousRecord?.split('\n', limit = 2)
    return shouldResendLanEndpoint(
        previous?.getOrNull(0),
        previous?.getOrNull(1)?.toLongOrNull(),
        currentSignature,
        nowMs,
    )
}

internal fun lanEndpointSendRecord(signature: String, sentAtMs: Long): String =
    "$signature\n$sentAtMs"
