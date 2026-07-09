package com.cruisemesh.app.relay

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

/**
 * Thin HTTPS client for `cruisemesh-relayd`. It moves only the §6.4 public
 * envelope header shape plus the sealed bytes; plaintext message metadata
 * never crosses this boundary.
 */
object RelayClient {
    private val gson = Gson()

    fun postOutboundEnvelope(config: RelayConfig, envelope: OutboundEnvelope): Long =
        postEnvelope(
            config,
            msgId = envelope.msgId,
            hopTtl = envelope.hopTtl,
            recipientHint = envelope.recipientHint,
            sealed = envelope.sealed,
            expiryMs = envelope.expiry,
        )

    fun postCarriedEnvelope(config: RelayConfig, envelope: CarriedEnvelope): Long =
        postEnvelope(
            config,
            msgId = envelope.msgId,
            hopTtl = envelope.hopTtl,
            recipientHint = envelope.recipientHint,
            sealed = envelope.sealed,
            expiryMs = envelope.expiry,
        )

    fun postReceiptEnvelope(config: RelayConfig, envelope: OutgoingReceiptEnvelope): Long =
        postEnvelope(
            config,
            msgId = envelope.msgId,
            hopTtl = envelope.hopTtl,
            recipientHint = envelope.recipientHint,
            sealed = envelope.sealed,
            expiryMs = envelope.expiry,
        )

    fun fetchEnvelopes(
        config: RelayConfig,
        hints: List<ByteArray>,
        afterId: Long,
        limit: Int,
    ): RelayFetchPage {
        val encodedHints = hints.joinToString(",") { urlEncode(base64Url(it)) }
        val url = buildUrl(config.relayUrl, "/envelopes?hints=$encodedHints&after=$afterId&limit=$limit")
        val connection = openConnection(url, "GET", config)
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

    fun ackEnvelopes(config: RelayConfig, ids: List<Long>) {
        if (ids.isEmpty()) return
        val body = gson.toJson(AckRequest(ids))
        val connection = openConnection(buildUrl(config.relayUrl, "/envelopes/ack"), "POST", config)
        connection.writeJson(body)
        connection.useJsonResponse { }
    }

    private fun postEnvelope(
        config: RelayConfig,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        sealed: ByteArray,
        expiryMs: Long,
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
        val connection = openConnection(buildUrl(config.relayUrl, "/envelopes"), "POST", config)
        connection.writeJson(body)
        return connection.useJsonResponse { gson.fromJson(it, PostEnvelopeResponse::class.java).id }
    }

    private fun openConnection(url: String, method: String, config: RelayConfig): HttpURLConnection {
        val connection = URL(url).openConnection() as HttpURLConnection
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
        val trimmed = baseUrl.trim().trimEnd('/')
        return "$trimmed$pathAndQuery"
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
