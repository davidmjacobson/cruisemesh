package com.cruisemesh.app.relay

import android.net.Network
import com.google.gson.Gson
import com.google.gson.annotations.SerializedName
import uniffi.cruisemesh_core.CarriedEnvelope
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.OutgoingReceiptEnvelope
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import java.util.Base64

private const val CONNECT_TIMEOUT_MS = 10_000
private const val READ_TIMEOUT_MS = 10_000
private const val RELAY_USER_AGENT = "CruiseMeshRelayClient/0.1"
private const val RELAY_BYPASS_TUNNEL_REMINDER = "1"

data class RelayFetchedEnvelope(
    val id: Long,
    val msgId: ByteArray,
    val hopTtl: UByte,
    val recipientHint: ByteArray,
    val sealed: ByteArray,
    val expiryMs: Long,
)

data class RelayFetchPage(
    val envelopes: List<RelayFetchedEnvelope>,
    val nextCursor: Long,
)

data class RelayPresence(
    val hint: ByteArray,
    val lastSeenMs: Long,
)

data class RelayPresencePage(
    val nowMs: Long,
    val presence: List<RelayPresence>,
)

/**
 * Thin HTTPS client for `cruisemesh-relayd`. It moves only the §6.4 public
 * envelope header shape plus the sealed bytes; plaintext message metadata
 * never crosses this boundary.
 */
object RelayClient {
    private val gson = Gson()

    fun postOutboundEnvelope(config: RelayConfig, envelope: OutboundEnvelope, network: Network? = null): Long =
        postEnvelope(
            config,
            msgId = envelope.msgId,
            hopTtl = envelope.hopTtl,
            recipientHint = envelope.recipientHint,
            sealed = envelope.sealed,
            expiryMs = envelope.expiry,
            network = network,
        )

    fun postCarriedEnvelope(config: RelayConfig, envelope: CarriedEnvelope, network: Network? = null): Long =
        postEnvelope(
            config,
            msgId = envelope.msgId,
            hopTtl = envelope.hopTtl,
            recipientHint = envelope.recipientHint,
            sealed = envelope.sealed,
            expiryMs = envelope.expiry,
            network = network,
        )

    fun postReceiptEnvelope(config: RelayConfig, envelope: OutgoingReceiptEnvelope, network: Network? = null): Long =
        postEnvelope(
            config,
            msgId = envelope.msgId,
            hopTtl = envelope.hopTtl,
            recipientHint = envelope.recipientHint,
            sealed = envelope.sealed,
            expiryMs = envelope.expiry,
            network = network,
        )

    fun fetchEnvelopes(
        config: RelayConfig,
        hints: List<ByteArray>,
        afterId: Long,
        limit: Int,
        network: Network? = null,
    ): RelayFetchPage {
        val encodedHints = hints.joinToString(",") { urlEncode(base64Url(it)) }
        val url = buildUrl(config.relayUrl, "/envelopes?hints=$encodedHints&after=$afterId&limit=$limit")
        val connection = openConnection(url, "GET", config, network)
        return connection.useJsonResponse { body ->
            val response = gson.fromJson(body, GetEnvelopesResponse::class.java)
            RelayFetchPage(
                envelopes = response.envelopes.map { item ->
                    RelayFetchedEnvelope(
                        id = item.id,
                        msgId = base64UrlDecode(item.msgId),
                        hopTtl = item.hopTtl.toUByte(),
                        recipientHint = base64UrlDecode(item.recipientHint),
                        sealed = base64UrlDecode(item.sealed),
                        expiryMs = item.expiryMs,
                    )
                },
                nextCursor = response.nextCursor,
            )
        }
    }

    fun ackEnvelopes(config: RelayConfig, ids: List<Long>, network: Network? = null) {
        if (ids.isEmpty()) return
        val body = gson.toJson(AckRequest(ids))
        val connection = openConnection(buildUrl(config.relayUrl, "/envelopes/ack"), "POST", config, network)
        connection.writeJson(body)
        connection.useJsonResponse { }
    }

    fun syncPresence(
        config: RelayConfig,
        announce: List<ByteArray>,
        query: List<ByteArray>,
        network: Network? = null,
    ): RelayPresencePage {
        val body = gson.toJson(
            PresenceRequest(
                announce = announce.map(::base64Url),
                query = query.map(::base64Url),
            ),
        )
        val connection = openConnection(buildUrl(config.relayUrl, "/presence"), "POST", config, network)
        connection.writeJson(body)
        return connection.useJsonResponse { responseBody ->
            val response = gson.fromJson(responseBody, PresenceResponse::class.java)
            RelayPresencePage(
                nowMs = response.nowMs,
                presence = response.presence.map { item ->
                    RelayPresence(
                        hint = base64UrlDecode(item.hint),
                        lastSeenMs = item.lastSeenMs,
                    )
                },
            )
        }
    }

    private fun postEnvelope(
        config: RelayConfig,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        sealed: ByteArray,
        expiryMs: Long,
        network: Network?,
    ): Long {
        val body = gson.toJson(
            PostEnvelopeRequest(
                msgId = base64Url(msgId),
                hopTtl = hopTtl.toInt(),
                recipientHint = base64Url(recipientHint),
                sealed = base64Url(sealed),
                expiryMs = expiryMs,
            ),
        )
        val connection = openConnection(buildUrl(config.relayUrl, "/envelopes"), "POST", config, network)
        connection.writeJson(body)
        return connection.useJsonResponse { gson.fromJson(it, PostEnvelopeResponse::class.java).id }
    }

    /**
     * Opens an [HttpURLConnection] to the relay. When [network] is non-null the
     * connection is pinned to that specific [Network] via
     * [Network.openConnection] instead of the process default. This is what lets
     * relay sync ride a validated network (e.g. cellular) even while Android
     * still lists an associated-but-dead Wi‑Fi as the system default network.
     */
    private fun openConnection(url: String, method: String, config: RelayConfig, network: Network?): HttpURLConnection {
        val parsed = URL(url)
        val connection = (network?.openConnection(parsed) ?: parsed.openConnection()) as HttpURLConnection
        connection.requestMethod = method
        connection.connectTimeout = CONNECT_TIMEOUT_MS
        connection.readTimeout = READ_TIMEOUT_MS
        connection.setRequestProperty("Authorization", "Bearer ${config.relayToken}")
        connection.setRequestProperty("Accept", "application/json")
        connection.setRequestProperty("User-Agent", RELAY_USER_AGENT)
        connection.setRequestProperty("bypass-tunnel-reminder", RELAY_BYPASS_TUNNEL_REMINDER)
        return connection
    }

    private fun HttpURLConnection.writeJson(body: String) {
        doOutput = true
        setRequestProperty("Content-Type", "application/json")
        outputStream.use { it.write(body.toByteArray(StandardCharsets.UTF_8)) }
    }

    private inline fun <T> HttpURLConnection.useJsonResponse(block: (String) -> T): T {
        return try {
            val code = responseCode
            val stream = if (code in 200..299) inputStream else errorStream
            val body = stream?.bufferedReader(StandardCharsets.UTF_8)?.use { it.readText() }.orEmpty()
            if (code !in 200..299) {
                throw IOException("Relay request failed ($code): $body")
            }
            block(body)
        } finally {
            disconnect()
        }
    }

    private fun buildUrl(baseUrl: String, pathAndQuery: String): String {
        return "${normalizeRelayUrl(baseUrl)}$pathAndQuery"
    }

    private fun base64Url(bytes: ByteArray): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    private fun base64UrlDecode(value: String): ByteArray =
        Base64.getUrlDecoder().decode(value)

    private fun urlEncode(value: String): String =
        URLEncoder.encode(value, StandardCharsets.UTF_8)

    private data class PostEnvelopeRequest(
        @SerializedName("msg_id") val msgId: String,
        @SerializedName("hop_ttl") val hopTtl: Int,
        @SerializedName("recipient_hint") val recipientHint: String,
        @SerializedName("sealed") val sealed: String,
        @SerializedName("expiry_ms") val expiryMs: Long,
    )

    private data class PostEnvelopeResponse(
        @SerializedName("id") val id: Long,
    )

    private data class AckRequest(
        @SerializedName("ids") val ids: List<Long>,
    )

    private data class PresenceRequest(
        @SerializedName("announce") val announce: List<String>,
        @SerializedName("query") val query: List<String>,
    )

    private data class PresenceResponse(
        @SerializedName("now_ms") val nowMs: Long,
        @SerializedName("presence") val presence: List<PresencePayload>,
    )

    private data class PresencePayload(
        @SerializedName("hint") val hint: String,
        @SerializedName("last_seen_ms") val lastSeenMs: Long,
    )

    private data class GetEnvelopesResponse(
        @SerializedName("envelopes") val envelopes: List<EnvelopePayload>,
        @SerializedName("next_cursor") val nextCursor: Long,
    )

    private data class EnvelopePayload(
        @SerializedName("id") val id: Long,
        @SerializedName("msg_id") val msgId: String,
        @SerializedName("hop_ttl") val hopTtl: Int,
        @SerializedName("recipient_hint") val recipientHint: String,
        @SerializedName("sealed") val sealed: String,
        @SerializedName("expiry_ms") val expiryMs: Long,
    )
}
