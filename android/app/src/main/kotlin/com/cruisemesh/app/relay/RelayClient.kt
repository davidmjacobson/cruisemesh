package com.cruisemesh.app.relay

import android.net.Network
import uniffi.cruisemesh_core.CarriedEnvelope
import uniffi.cruisemesh_core.CoreGroupFanoutRow
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.OutgoingReceiptEnvelope
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.nio.charset.StandardCharsets
import uniffi.cruisemesh_core.relayBuildFetchPath
import uniffi.cruisemesh_core.relayDecodeFetchPage
import uniffi.cruisemesh_core.relayDecodePostResponse
import uniffi.cruisemesh_core.relayDecodePresencePage
import uniffi.cruisemesh_core.relayEncodeAckRequest
import uniffi.cruisemesh_core.relayEncodePostEnvelope
import uniffi.cruisemesh_core.relayEncodePresenceRequest

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

    /**
     * Posts one per-member fan-out row of a group message
     * (specs/group-relay-durability.md §4; built by the core's
     * `coreGroupFanoutRows`/`coreGroupFanoutRowsForCarried`). Same wire shape
     * as every other envelope post -- fan-out changes addressing, not format.
     */
    fun postFanoutRow(config: RelayConfig, row: CoreGroupFanoutRow, network: Network? = null): Long =
        postEnvelope(
            config,
            msgId = row.msgId,
            hopTtl = row.hopTtl,
            recipientHint = row.recipientHint,
            sealed = row.sealed,
            expiryMs = row.expiry,
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
        val url = buildUrl(config.relayUrl, relayBuildFetchPath(hints, afterId, limit.toUInt()))
        val connection = openConnection(url, "GET", config, network)
        return connection.useJsonResponse { body ->
            val response = relayDecodeFetchPage(body.toByteArray(StandardCharsets.UTF_8))
            RelayFetchPage(
                envelopes = response.envelopes.map { item ->
                    RelayFetchedEnvelope(
                        id = item.id,
                        msgId = item.msgId,
                        hopTtl = item.hopTtl,
                        recipientHint = item.recipientHint,
                        sealed = item.sealed,
                        expiryMs = item.expiryMs,
                    )
                },
                nextCursor = response.nextCursor,
            )
        }
    }

    fun ackEnvelopes(config: RelayConfig, ids: List<Long>, network: Network? = null) {
        if (ids.isEmpty()) return
        val body = relayEncodeAckRequest(ids)
        val connection = openConnection(buildUrl(config.relayUrl, "/envelopes/ack"), "POST", config, network)
        connection.writeJson(String(body, StandardCharsets.UTF_8))
        connection.useJsonResponse { }
    }

    fun syncPresence(
        config: RelayConfig,
        announce: List<ByteArray>,
        query: List<ByteArray>,
        network: Network? = null,
    ): RelayPresencePage {
        val body = relayEncodePresenceRequest(announce, query)
        val connection = openConnection(buildUrl(config.relayUrl, "/presence"), "POST", config, network)
        connection.writeJson(String(body, StandardCharsets.UTF_8))
        return connection.useJsonResponse { responseBody ->
            val response = relayDecodePresencePage(responseBody.toByteArray(StandardCharsets.UTF_8))
            RelayPresencePage(
                nowMs = response.nowMs,
                presence = response.presence.map { item ->
                    RelayPresence(
                        hint = item.hint,
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
        val body = relayEncodePostEnvelope(msgId, hopTtl, recipientHint, sealed, expiryMs)
        val connection = openConnection(buildUrl(config.relayUrl, "/envelopes"), "POST", config, network)
        connection.writeJson(String(body, StandardCharsets.UTF_8))
        return connection.useJsonResponse { relayDecodePostResponse(it.toByteArray(StandardCharsets.UTF_8)) }
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

}
