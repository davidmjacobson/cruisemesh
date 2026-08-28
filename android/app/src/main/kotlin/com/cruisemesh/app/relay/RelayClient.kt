package com.cruisemesh.app.relay

import android.net.Network
import android.util.Log
import com.google.gson.JsonParser
import uniffi.cruisemesh_core.CarriedEnvelope
import uniffi.cruisemesh_core.CoreGroupFanoutRow
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.OutgoingReceiptEnvelope
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.SocketTimeoutException
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
import uniffi.cruisemesh_core.relayRotatePath

private const val CONNECT_TIMEOUT_MS = 10_000

/**
 * Per-read inactivity budget, not a budget for the whole transfer: a full
 * fetch page can be megabytes, and a link slow enough to need a minute for it
 * is slow, not broken. [HttpURLConnection.setReadTimeout] resets on every
 * read, so a download that keeps trickling in is never cut off. Matches iOS,
 * whose relay client waits on progress rather than on the clock.
 */
private const val READ_TIMEOUT_MS = 10_000

/**
 * How much of a non-2xx body is kept -- enough to quote the relay's reason in
 * the error, never enough for an error page to cost memory.
 */
internal const val ERROR_BODY_PREVIEW_BYTES = 2_048
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
 *
 * The response body is not among them, in the message or in a field. Callers
 * log [message] as-is, so anything on here is content the far end chose and
 * this app then wrote into a file a user shares. Mirrors iOS
 * `RelayHTTPError`.
 */
class RelayHttpException(
    val code: Int,
    val relayCode: String?,
    message: String,
    val retryAfter: String? = null,
) : IOException(message)

/**
 * A fetch failure that asking for fewer rows can fix.
 *
 * Its own type rather than a bare [IOException] because these are the
 * transport failures a caller can actually do something about, as opposed to
 * the ones that only mean "try again later": a page can be too big to decode,
 * or too big to finish moving over the link it was asked for, and both are
 * answered the same way -- same cursor, smaller window (see
 * [RelayClient.fetchEnvelopesWithinResponseCap]). Mirrors iOS
 * `RelayPageTooBigError`.
 */
sealed class RelayPageTooBigException(message: String, cause: Throwable? = null) :
    IOException(message, cause)

/**
 * The relay's answer was larger than [relayMaxResponseBytes], so it was
 * refused before the whole thing could be accumulated.
 */
class RelayResponseTooLargeException(val maxBytes: Int) :
    RelayPageTooBigException("Relay response exceeds $maxBytes bytes")

/**
 * The relay answered, and then the body stopped arriving before the end.
 *
 * On a ship's Wi-Fi a full page can be megabytes, and a link that cannot carry
 * it inside the read timeout will fail the same way on the next pass, from the
 * same cursor -- the same permanent stall an undecodable page causes. So it
 * gets the same treatment: ask for fewer rows and let the mail through slowly
 * rather than not at all. Distinguished from a timeout while connecting or
 * waiting for the response head, which says nothing about page size. Mirrors
 * iOS `RelayResponseStalledError`.
 */
class RelayResponseStalledException(val bytesReceived: Int, cause: Throwable?) :
    RelayPageTooBigException("Relay response stalled after $bytesReceived bytes", cause)

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
    /**
     * Every relay call lands here, and until now none of them left a trace.
     *
     * That is the single biggest hole in a shared diagnostics log: the relay is
     * where this app's hardest bugs have lived -- 401s against a stale contact
     * endpoint, 429 storms, silent-host demotion, a re-upload loop that
     * bypassed Retry-After -- and every one had to be reproduced locally
     * because the tester's log said nothing about the relay at all.
     */
    const val TAG = "RelayClient"

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
     * the relay's answer is too big for this client to take -- either too big
     * to decode, or too big to finish moving over this link. Returns the page
     * together with the limit that actually produced it.
     *
     * The stall this prevents: `limit` bounds a page's row count, not its
     * size, and one sealed payload may be 512 KiB. A mailbox holding enough
     * large attachment chunks can therefore produce a full-size window whose
     * body is past [relayMaxResponseBytes], or simply past what a ship's Wi-Fi
     * will carry before the read times out. Without a retry the pass simply
     * fails there; the next pass asks the same relay for the same window from
     * the same cursor and fails identically, so the frontier never advances
     * and nothing behind those rows is delivered until they expire.
     *
     * Current relayd carries a byte budget and never builds an undecodable
     * page, but family relays are self-hosted and older builds exist in the
     * field, so the client cannot assume the server-side fix is there -- and
     * no server-side budget can make a slow link fast.
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
            } catch (e: RelayPageTooBigException) {
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

    /**
     * §10 step 2: ask the family relay to re-key itself.
     *
     * The bearer is a whole credential rather than the saved one because a
     * rotation is the one call that may be made under *either* of two tokens:
     * the retired one on the first ask, and the replacement when the first ask
     * says the retired one is no longer this family's. [RelayRotationDriver]
     * owns that choice; this only carries it.
     *
     * The body is opaque here on purpose. It is signed, and core signs it
     * (`relayEncodeRotateRequest`) — a shell that assembled these bytes could
     * get the signed message wrong in a way no test on this side would catch,
     * and the answer is likewise handed straight back for
     * `relayDecodeRotateResponse` to check against the token that was asked
     * for. Mirrors iOS `RelayClient.rotateFamilyToken`.
     */
    fun rotateFamilyToken(config: RelayConfig, body: ByteArray, network: Network? = null): ByteArray {
        val connection = openConnection(buildUrl(config.relayUrl, relayRotatePath()), "POST", config, network)
        connection.writeJson(String(body, StandardCharsets.UTF_8))
        return connection.useJsonResponse { it }
    }

    /**
     * The one place an envelope becomes an HTTP POST. Internal rather than
     * private because a device-link rendezvous
     * ([com.cruisemesh.app.devicelink.LinkRelayWire]) posts a row that belongs
     * to no conversation and no contact: it has a hint, a body and an expiry,
     * and none of the addressing an [OutboundEnvelope] carries.
     */
    internal fun postEnvelope(
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
        val connection = openTransport(url, method, network)
        connection.setRequestProperty("Authorization", "Bearer ${config.relayToken}")
        connection.setRequestProperty("Accept", "application/json")
        return connection
    }

    /**
     * The transport substrate, with no protocol opinion in it: the socket, the
     * [Network] pin, the timeouts, and the two headers that belong to this
     * HTTP client rather than to the relay protocol -- the user agent that
     * identifies the client, and the tunnel-bypass hint a development relay
     * needs.
     *
     * Split out so a second engine cannot end up with a second transport. The
     * core relay driver forms its own protocol headers (core decides those)
     * and gets everything below them from here, which is what makes "the two
     * engines put the same bytes on the wire" a property of there being one
     * connection opener rather than of two files agreeing.
     */
    internal fun openTransport(url: String, method: String, network: Network?): HttpURLConnection {
        val parsed = URL(url)
        val connection = (network?.openConnection(parsed) ?: parsed.openConnection()) as HttpURLConnection
        connection.requestMethod = method
        connection.connectTimeout = CONNECT_TIMEOUT_MS
        connection.readTimeout = READ_TIMEOUT_MS
        connection.setRequestProperty("User-Agent", RELAY_USER_AGENT)
        connection.setRequestProperty("bypass-tunnel-reminder", RELAY_BYPASS_TUNNEL_REMINDER)
        return connection
    }

    private fun HttpURLConnection.writeJson(body: String) {
        doOutput = true
        setRequestProperty("Content-Type", "application/json")
        outputStream.use { it.write(body.toByteArray(StandardCharsets.UTF_8)) }
    }

    /**
     * One line per relay call.
     *
     * Only the URL *path* is logged, never the query: the fetch path carries
     * recipient hints, and this log gets shared with whoever is helping.
     */
    fun logOutcome(method: String, path: String, code: Int, ms: Long, bytes: Int) {
        Log.i(TAG, "$method $path -> $code in ${ms}ms, ${bytes}B")
    }

    fun logFailure(method: String, path: String, ms: Long, detail: String) {
        Log.e(TAG, "$method $path failed after ${ms}ms: $detail")
    }

    private inline fun <T> HttpURLConnection.useJsonResponse(block: (ByteArray) -> T): T {
        val started = System.currentTimeMillis()
        val method = requestMethod ?: "?"
        val path = runCatching { url.path }.getOrNull() ?: "?"
        return try {
            val code = responseCode
            val maxBytes = relayMaxResponseBytes().toInt()
            // Status before size. A captive portal notice, a proxy banner or a
            // gateway error page can be any size at all, and calling one an
            // oversized *page* sends a fetch down the shrink ladder -- eight
            // more round trips that were never going to succeed -- and throws
            // away a 429's Retry-After on the way. An error body is only ever
            // read to name the failure, so only a preview of it is taken, and
            // a body that will not finish arriving does not hide the status.
            if (code !in 200..299) {
                val previewBytes =
                    runCatching { errorStream?.use { it.readAtMost(ERROR_BODY_PREVIEW_BYTES) } }
                        .getOrNull() ?: ByteArray(0)
                val preview = String(previewBytes, StandardCharsets.UTF_8)
                val relayCode = runCatching {
                    JsonParser.parseString(preview).asJsonObject.get("code")?.asString
                }.getOrNull()
                val semantic = relayCode?.let { " [$it]" }.orEmpty()
                val retryAfter = getHeaderField("Retry-After")
                // The fields that explain a stuck relay: relayd's own
                // machine-readable reason, and the header the carry re-upload
                // storm ignored.
                Log.e(
                    TAG,
                    "$method $path -> $code${semantic.ifEmpty { " [-]" }} " +
                        "in ${System.currentTimeMillis() - started}ms " +
                        "retryAfter=${retryAfter ?: "-"}",
                )
                // `preview` ends here. It was read to name the failure, and
                // `relayCode` is that name; the bytes themselves are whatever
                // the far end chose to send -- a captive-portal page, a proxy
                // banner, a gateway error -- and this exception's message is
                // logged verbatim at a dozen call sites in RelaySyncEngine and
                // again with the throwable during Shore Pass setup. It is not
                // kept as an unlogged field either: nothing reads one, and a
                // field that exists is one `Log.w(TAG, msg, e)` away from being
                // in the archive anyway. What survives is what triage used:
                // status, relayd's code, and whether a body came back at all.
                throw RelayHttpException(
                    code,
                    relayCode,
                    "Relay request failed ($code)${semantic.ifEmpty { " [-]" }} " +
                        "body=${previewBytes.size}" +
                        (if (previewBytes.size >= ERROR_BODY_PREVIEW_BYTES) "+" else "") + "B",
                    retryAfter = retryAfter,
                )
            }
            if (contentLengthLong > maxBytes) {
                throw RelayResponseTooLargeException(maxBytes)
            }
            val body = inputStream?.use { it.readBounded(maxBytes) } ?: ByteArray(0)
            logOutcome(method, path, code, System.currentTimeMillis() - started, body.size)
            block(body)
        } catch (e: RelayHttpException) {
            // Already logged above with its status and Retry-After; re-logging
            // here would double every relay failure in the shared archive.
            throw e
        } catch (e: Exception) {
            // Transport and decode failures never reach the branch above, so
            // this is their only chance to be recorded.
            logFailure(method, path, System.currentTimeMillis() - started, "${e.javaClass.simpleName}: ${e.message}")
            throw e
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

/**
 * Reads a relay response body, refusing anything past [maxBytes].
 *
 * A read that times out part-way is reported as a
 * [RelayResponseStalledException] rather than a bare [SocketTimeoutException]:
 * by this point the relay has answered and the head is in, so what stalled is
 * the body -- a page this link will not carry, which the same window from the
 * same cursor will not carry next pass either. The fetch walk recovers by
 * asking for fewer rows. A timeout before the head (while connecting, or
 * waiting on the status line) never reaches here and stays a plain
 * [SocketTimeoutException]: nothing about it says the page was too big.
 */
internal fun InputStream.readBounded(maxBytes: Int): ByteArray {
    require(maxBytes >= 0) { "maxBytes must be non-negative" }
    val output = ByteArrayOutputStream(minOf(maxBytes, 8 * 1024))
    val buffer = ByteArray(8 * 1024)
    var total = 0
    while (true) {
        val read = try {
            read(buffer)
        } catch (e: SocketTimeoutException) {
            throw RelayResponseStalledException(total, e)
        }
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

/**
 * Reads at most [maxBytes] and stops, leaving the rest unread. Used only for
 * the preview of a non-2xx body: enough to quote the relay's reason, never
 * enough for an error page to cost memory.
 */
internal fun InputStream.readAtMost(maxBytes: Int): ByteArray {
    require(maxBytes >= 0) { "maxBytes must be non-negative" }
    val output = ByteArrayOutputStream(minOf(maxBytes, 8 * 1024))
    val buffer = ByteArray(minOf(maxBytes, 8 * 1024).coerceAtLeast(1))
    var total = 0
    while (total < maxBytes) {
        val read = read(buffer, 0, minOf(buffer.size, maxBytes - total))
        if (read < 0) break
        if (read == 0) continue
        output.write(buffer, 0, read)
        total += read
    }
    return output.toByteArray()
}
