package com.cruisemesh.app.relay

import android.net.Network
import android.util.Log
import uniffi.cruisemesh_core.CoreRelayHeader
import uniffi.cruisemesh_core.CoreRelayHttpRequest
import uniffi.cruisemesh_core.CoreRelayHttpResult
import uniffi.cruisemesh_core.CoreRelayTransportError
import java.io.IOException
import java.net.HttpURLConnection
import java.net.SocketTimeoutException
import javax.net.ssl.SSLException

/**
 * Executes one typed relay action and reports what happened. Nothing else.
 *
 * The core relay session decides everything about a request -- method, path,
 * every header including `Authorization`, the body bytes, how much of a
 * response may be read, and which response headers it wants back. This class
 * puts exactly that on the wire and hands back a bounded, typed result. It
 * does not retry, does not shrink a page, does not decide whether a row may be
 * acked, does not advance a cursor, does not interpret a status code, and does
 * not know what a pass is. Every one of those is a decision that needs state
 * this class deliberately cannot see.
 *
 * That restraint is the whole point of the seam. A driver that inferred even
 * one of them would be a second place the protocol is decided, and the second
 * place is always the one nobody updates.
 *
 * # What it does own
 *
 * Three things, and they are all Android's:
 *
 * **The socket, and which network it rides.** [RelayClient.openTransport] is
 * shared with the legacy engine on purpose -- the [Network] pin, the timeouts,
 * and the client's own transport headers come from one place, so the two
 * engines cannot drift apart at the transport layer. The network to pin to is
 * the caller's decision (`MeshService`/`RelaySyncEngine` resolve it), because
 * whether a default route is trustworthy is a question only the platform can
 * answer: relay sync rides a validated network when the default is an
 * associated-but-dead Wi-Fi, and never binds past a VPN, where binding is
 * both forbidden and a tunnel bypass.
 *
 * **The response cap.** Core declares
 * [CoreRelayHttpRequest.maxResponseBytes]; this enforces it with a bounded
 * read that stops rather than allocating. Exceeding it is reported as
 * [CoreRelayTransportError.BODY_TOO_LARGE], which core answers by asking for
 * fewer rows from the same cursor -- so this must not be reported for anything
 * that is *not* a page too big to take, or a fetch is sent down a shrink
 * ladder that was never going to help.
 *
 * **Which failure it was.** A socket that never connected, a TLS handshake
 * that failed, a body that stopped arriving, a cancellation: core folds these
 * into health and silence evidence, so they must arrive as distinct typed
 * values rather than as one "it failed".
 *
 * # Status before size
 *
 * A non-2xx body is read as a short preview and never as an oversize failure,
 * which the legacy client learned the hard way: a captive-portal notice, a
 * proxy banner or a gateway error page can be any size at all, and calling one
 * an oversized *page* both sends a fetch down the shrink ladder and throws
 * away the `Retry-After` header on a 429 -- the one header `RATE-01` is
 * measured from.
 */
object CoreRelayDriver {

    private const val TAG = "MeshService"

    /**
     * Run one action.
     *
     * Never throws: every failure is a typed result, because a driver that
     * threw would make the session's "one outstanding action" invariant the
     * caller's problem to unwind.
     *
     * @param nowMs the wall clock to stamp the result with, supplied rather
     *   than read so a test can run a pass on a fake clock and so the session
     *   is never handed a time this class chose.
     * @param isCancelled consulted before the request is issued and again
     *   after it completes; the process going away mid-pass is a cancellation,
     *   not an outage, and telling core otherwise would let a backgrounded app
     *   accumulate silence evidence against healthy endpoints.
     */
    fun execute(
        passId: String,
        actionId: ULong,
        request: CoreRelayHttpRequest,
        network: Network?,
        nowMs: Long,
        isCancelled: () -> Boolean = { false },
    ): CoreRelayHttpResult {
        if (isCancelled()) {
            return failure(passId, actionId, CoreRelayTransportError.CANCELLED, nowMs)
        }
        val url = request.baseUrl + request.path
        val started = System.currentTimeMillis()
        var connection: HttpURLConnection? = null
        return try {
            val opened = RelayClient.openTransport(url, request.method, network)
            connection = opened
            for (header in request.headers) {
                opened.setRequestProperty(header.name, header.value)
            }
            if (request.body.isNotEmpty()) {
                opened.doOutput = true
                opened.outputStream.use { it.write(request.body) }
            }
            val result = read(passId, actionId, request, opened, nowMs)
            if (isCancelled()) {
                failure(passId, actionId, CoreRelayTransportError.CANCELLED, nowMs)
            } else {
                result
            }
        } catch (e: Exception) {
            val error = relayClassifyTransportError(e)
            // The path only, never the query: a fetch path carries recipient
            // hints and this log is shared with whoever is helping.
            RelayClient.logFailure(
                request.method,
                request.path.substringBefore('?'),
                System.currentTimeMillis() - started,
                "${e.javaClass.simpleName}: ${e.message}",
            )
            failure(passId, actionId, error, nowMs)
        } finally {
            connection?.disconnect()
        }
    }

    private fun read(
        passId: String,
        actionId: ULong,
        request: CoreRelayHttpRequest,
        connection: HttpURLConnection,
        nowMs: Long,
    ): CoreRelayHttpResult {
        val started = System.currentTimeMillis()
        val code = connection.responseCode
        val cap = request.maxResponseBytes.toInt()
        val body = if (code in 200..299) {
            // Declared length first: a server that announces an oversize body
            // is refused before a byte of it is read.
            if (connection.contentLengthLong > cap) throw RelayResponseTooLargeException(cap)
            connection.inputStream?.use { it.readBounded(cap) } ?: ByteArray(0)
        } else {
            // Status before size. An error body is only ever read so core can
            // name the failure from the relay's own stable code, so a preview
            // is enough and an unfinishable one must not hide the status.
            runCatching { connection.errorStream?.use { it.readAtMost(ERROR_BODY_PREVIEW_BYTES) } }
                .getOrNull() ?: ByteArray(0)
        }
        RelayClient.logOutcome(
            request.method,
            request.path.substringBefore('?'),
            code,
            System.currentTimeMillis() - started,
            body.size,
        )
        return CoreRelayHttpResult(
            passId = passId,
            actionId = actionId,
            status = code.toUShort(),
            headers = selectedHeaders(request, connection),
            body = body,
            error = null,
            completedAtMs = nowMs,
        )
    }

    /**
     * Only the headers core asked for.
     *
     * Everything else is dropped here rather than passed along and ignored:
     * a response header core never requested cannot reach a store, an event or
     * a summary if it never crosses the boundary in the first place.
     */
    private fun selectedHeaders(
        request: CoreRelayHttpRequest,
        connection: HttpURLConnection,
    ): List<CoreRelayHeader> = request.responseHeadersWanted.mapNotNull { name ->
        connection.getHeaderField(name)?.let { CoreRelayHeader(name, it) }
    }

    private fun failure(
        passId: String,
        actionId: ULong,
        error: CoreRelayTransportError,
        nowMs: Long,
    ): CoreRelayHttpResult = CoreRelayHttpResult(
        passId = passId,
        actionId = actionId,
        // Zero, not a synthesized status: core distinguishes "the relay
        // answered badly" from "there was no answer", and the second must not
        // be able to masquerade as the first.
        status = 0u,
        headers = emptyList(),
        body = ByteArray(0),
        error = error,
        completedAtMs = nowMs,
    )
}

/**
 * Which typed failure a thrown exception was.
 *
 * A free function rather than a member of [CoreRelayDriver], and that is not
 * tidiness. The migration canary needs this mapping to describe a failure the
 * *legacy* engine saw, and reaching it through the driver would put the object
 * that opens sockets inside the canary's reach -- one line from a comparison
 * to a live request. The classification is a pure function of an exception and
 * belongs to neither engine, so it lives beside both.
 *
 * [RelayPageTooBigException] maps to the same answer as an oversize body on
 * purpose: the relay answered and then the body stopped arriving, which on a
 * link that will not carry a full page is the same permanent stall from the
 * same cursor, and asking for fewer rows is the same fix. A timeout while
 * *connecting* or waiting for the status line says nothing about page size and
 * stays a plain timeout.
 */
internal fun relayClassifyTransportError(error: Exception): CoreRelayTransportError = when (error) {
    is RelayPageTooBigException -> CoreRelayTransportError.BODY_TOO_LARGE
    is SSLException -> CoreRelayTransportError.TLS
    is SocketTimeoutException -> CoreRelayTransportError.TIMEOUT
    is InterruptedException -> CoreRelayTransportError.CANCELLED
    is IOException -> CoreRelayTransportError.CONNECTION_FAILED
    else -> {
        Log.w("MeshService", "Relay saw an unexpected failure: ${error.javaClass.simpleName}")
        CoreRelayTransportError.OTHER
    }
}
