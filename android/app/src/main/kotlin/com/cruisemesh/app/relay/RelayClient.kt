package com.cruisemesh.app.relay

import android.net.Network
import com.google.gson.JsonParser
import uniffi.cruisemesh_core.CarriedEnvelope
import uniffi.cruisemesh_core.CoreGroupFanoutRow
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.OutgoingReceiptEnvelope
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
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
import uniffi.cruisemesh_core.relayFetchShrunkLimit
import uniffi.cruisemesh_core.relayMaxResponseBytes

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

/** A fetched page plus the row limit that actually produced it. */
data class RelayCappedFetch(
    val page: RelayFetchPage,
    val limit: Int,
)

/**
 * Relay HTTP failure carrying the status, relayd's stable error code, and --
 * for 429s -- the raw `Retry-After` header (CP2b; parsed/clamped by the
 * core's `relayRetryAfterMs`, never here).
 */
class RelayHttpException(
    val code: Int,
    val relayCode: String?,
    message: String,
    val retryAfter: String? = null,
) : IOException(message)

/**
 * The relay's answer was larger than [relayMaxResponseBytes], so it was
 * refused before the whole thing could be accumulated.
 *
 * Its own type rather than a bare [IOException] because it is the one
 * transport failure a caller can actually do something about: a fetch page
 * that blows the cap is recoverable by asking the same cursor for fewer rows
 * (see [RelayClient.fetchEnvelopesWithinResponseCap]). Every other
 * IOException here means "try again later"; this one means "ask for less".
 */
class RelayResponseTooLargeException(val maxBytes: Int) :
    IOException("Relay response exceeds $maxBytes bytes")

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
            val response = relayDecodeFetchPage(body)
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

    /**
     * Fetch one page, halving `limit` and retrying the *same* cursor whenever
     * the relay's answer is too big for this client to decode. Returns the
     * page together with the limit that actually produced it.
     *
     * The stall this prevents: `limit` bounds a page's row count, not its
     * size, and one sealed payload may be 512 KiB. A mailbox holding enough
     * large attachment chunks can therefore produce a full-size window whose
     * body is past [relayMaxResponseBytes]. Without a retry the pass simply
     * fails there; the next pass asks the same relay for the same window from
     * the same cursor and fails identically, so the frontier never advances
     * and nothing behind those rows is delivered until they expire.
     *
     * Current relayd carries a byte budget and never builds such a page, but
     * family relays are self-hosted and older builds exist in the field, so
     * the client cannot assume the server-side fix is there.
     *
     * `relayFetchShrunkLimit` returning null means one row was already the
     * ask: nothing smaller exists, so this is not a paging problem and the
     * failure is raised rather than retried forever.
     */
    fun fetchEnvelopesWithinResponseCap(
        config: RelayConfig,
        hints: List<ByteArray>,
        afterId: Long,
        limit: Int,
        network: Network? = null,
        onShrink: (Int, Int) -> Unit = { _, _ -> },
    ): RelayCappedFetch {
        var attempt = limit
        while (true) {
            try {
                return RelayCappedFetch(fetchEnvelopes(config, hints, afterId, attempt, network), attempt)
            } catch (e: RelayResponseTooLargeException) {
                val smaller = relayFetchShrunkLimit(attempt.toUInt())?.toInt() ?: throw e
                onShrink(attempt, smaller)
                attempt = smaller
            }
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
            val response = relayDecodePresencePage(responseBody)
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
        return connection.useJsonResponse { relayDecodePostResponse(it) }
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

    private inline fun <T> HttpURLConnection.useJsonResponse(block: (ByteArray) -> T): T {
        return try {
            val code = responseCode
            val maxBytes = relayMaxResponseBytes().toInt()
            if (contentLengthLong > maxBytes) {
                throw RelayResponseTooLargeException(maxBytes)
            }
            val stream = if (code in 200..299) inputStream else errorStream
            val body = stream?.use { it.readBounded(maxBytes) } ?: ByteArray(0)
            if (code !in 200..299) {
                val preview = String(body, 0, minOf(body.size, 2_048), StandardCharsets.UTF_8)
                val relayCode = runCatching {
                    JsonParser.parseString(preview).asJsonObject.get("code")?.asString
                }.getOrNull()
                val semantic = relayCode?.let { " [$it]" }.orEmpty()
                throw RelayHttpException(
                    code,
                    relayCode,
                    "Relay request failed ($code)$semantic: $preview",
                    retryAfter = getHeaderField("Retry-After"),
                )
            }
            block(body)
        } finally {
            disconnect()
        }
    }

    private fun buildUrl(baseUrl: String, pathAndQuery: String): String {
        // normalizeRelayUrl returns empty for a non-HTTPS base. Both callers
        // filter those out well before here (RelayConfigStore.load and
        // resolvedContactRelay both drop them), so this is the backstop that
        // keeps a future third caller from concatenating a bare path and
        // getting an opaque MalformedURLException instead of the reason.
        val base = normalizeRelayUrl(baseUrl)
        if (base.isEmpty()) {
            throw IOException("Relay URL must use https")
        }
        return "$base$pathAndQuery"
    }

}

internal fun InputStream.readBounded(maxBytes: Int): ByteArray {
    require(maxBytes >= 0) { "maxBytes must be non-negative" }
    val output = ByteArrayOutputStream(minOf(maxBytes, 8 * 1024))
    val buffer = ByteArray(8 * 1024)
    var total = 0
    while (true) {
        val read = read(buffer)
        if (read < 0) break
        if (read == 0) continue
        if (read > maxBytes - total) {
            throw RelayResponseTooLargeException(maxBytes)
        }
        output.write(buffer, 0, read)
        total += read
    }
    return output.toByteArray()
}
