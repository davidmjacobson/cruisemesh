package com.cruisemesh.app.relay

import android.os.Handler
import android.util.Log
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import java.util.Base64
import java.util.concurrent.TimeUnit

private const val TAG = "RelayPushClient"
private const val CONNECT_TIMEOUT_MS = 10_000L

/**
 * FA3: [RelayPushClient.connect] hands hint computation off to an async
 * caller and resumes later in [RelayPushClient.finishConnect] -- by the time
 * that reply lands, [stop] or a newer [RelayPushClient.start] may have
 * already superseded the request that triggered it. Pure so the "is this
 * reply still current" decision is directly unit-testable without an Android
 * [Handler][android.os.Handler] or a real socket: apply the reply only while
 * still running ([stopped] false) and still targeting the same config the
 * reply was computed for ([desiredConfig] unchanged since the request went
 * out).
 */
internal fun isPushHintReplyCurrent(stopped: Boolean, desiredConfig: RelayConfig?, replyConfig: RelayConfig): Boolean =
    !stopped && desiredConfig == replyConfig

/**
 * Idle read timeout. relayd pings on an interval well inside this; a value
 * this generous just guards against a socket that has gone silent without
 * either side noticing (see relayd/src/lib.rs `WS_WRITE_TIMEOUT`, the
 * server-side mirror of this same "slow/dead peer" concern).
 */
private const val READ_TIMEOUT_MS = 90_000L

/**
 * Live low-latency companion to the poll/fetch path (see [RelayClient] and
 * `MeshService.performRelaySyncPass`): opens relayd's `GET /ws` broadcast
 * (relayd/src/lib.rs module docs, "WebSocket push") for the caller's own
 * relay config and, on every pushed envelope notification, invokes [onPush]
 * -- nothing else. This class never fetches, decodes, acks, or stores an
 * envelope itself; that stays the poll path's job. Its cursor and ack logic
 * remain the single source of truth for delivery -- this is purely a
 * "something landed in the mailbox, go run that path now" doorbell, so two
 * internet-connected phones don't have to wait out a 60s poll interval to
 * see each other's messages.
 *
 * Because the doorbell and the authoritative fetch are decoupled, calling
 * [onPush] once per message (rather than trying to interpret which envelope
 * arrived) is deliberately generous: [onPush] is expected to be
 * `MeshService.requestRelaySync`, whose in-flight/pending flags already
 * coalesce back-to-back calls into a single extra pass.
 *
 * Reconnects with capped exponential backoff ([RelayPushBackoff]) whenever
 * the socket drops, whether from a write-timeout eviction, a lagged-consumer
 * disconnect, or plain network loss -- relayd's docs call all of these
 * "reconnect and replay from your cursor," which for this class just means
 * "reconnect, and let the next push (if any) ask the poll path to sync."
 *
 * ### Hints are computed asynchronously (FA3)
 *
 * [start]'s `hintsProvider` doesn't return a hint list directly -- it's
 * handed a completion function and must call it exactly once, eventually,
 * from any thread. [MeshService]'s implementation dispatches the actual
 * computation (a `MessageStore` scan over every contact and group) onto its
 * own background executor rather than blocking whichever thread called
 * [connect] (originally [start] itself, or a delayed reconnect off
 * [mainHandler]'s looper -- both were the main thread). [connect] just
 * kicks the request off and returns; [finishConnect] resumes once the
 * caller replies, re-checking [stopped] and [desiredConfig] first in case
 * [stop] or a newer [start] landed while the computation was in flight.
 *
 * ### Network binding
 *
 * Unlike [RelayClient]'s HTTP calls, this does **not** pin the socket to
 * `MeshService.relayBindTarget()`'s validated network. Doing so with OkHttp
 * would mean deriving a custom `SocketFactory` from `Network.socketFactory`
 * and plumbing it through per-connect -- for a connection that exists purely
 * to shave latency off a fallback that already handles pinning correctly,
 * that complexity isn't worth it. In the one case this matters (the system
 * default network is an unvalidated, associated-but-dead Wi‑Fi and only a
 * `requestNetwork`-granted cellular network is actually validated) the WS
 * connect attempt over the default network will simply fail and keep
 * backing off; the 60s poll still rides the pinned network exactly as
 * before and remains the correctness backstop.
 */
class RelayPushClient(
    private val mainHandler: Handler,
    private val httpClient: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(CONNECT_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        .readTimeout(READ_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        .retryOnConnectionFailure(false) // we drive our own backoff/reconnect below
        .build(),
    private val onPush: () -> Unit,
) {
    private val backoff = RelayPushBackoff()

    @Volatile private var desiredConfig: RelayConfig? = null
    @Volatile private var hintsProvider: (((List<ByteArray>) -> Unit) -> Unit)? = null
    @Volatile private var webSocket: WebSocket? = null
    @Volatile private var stopped = true
    private var reconnectRunnable: Runnable? = null

    /**
     * (Re)starts the push subscription against [config], computing the
     * subscribed `hints=` set from [hintsProvider] on every (re)connect --
     * so a contact or group added after the socket is already open is picked
     * up the next time it reconnects, without this class needing its own
     * change-tracking. Safe to call repeatedly (e.g. from every
     * network-validation callback); a no-op if already started against an
     * equal [config].
     *
     * [hintsProvider] is handed a completion callback rather than returning a
     * list directly -- see the class doc's "Hints are computed
     * asynchronously" section. It must call that callback exactly once.
     */
    @Synchronized
    fun start(config: RelayConfig, hintsProvider: (onReady: (List<ByteArray>) -> Unit) -> Unit) {
        if (!stopped && desiredConfig == config) return
        stop()
        stopped = false
        desiredConfig = config
        this.hintsProvider = hintsProvider
        backoff.recordSuccess() // fresh target: start its backoff from the floor
        connect()
    }

    /** Closes the socket (if any) and cancels any pending reconnect. Idempotent. */
    @Synchronized
    fun stop() {
        stopped = true
        desiredConfig = null
        hintsProvider = null
        reconnectRunnable?.let { mainHandler.removeCallbacks(it) }
        reconnectRunnable = null
        webSocket?.cancel()
        webSocket = null
    }

    /**
     * Kicks off hint computation for [desiredConfig] and returns immediately
     * -- [finishConnect] does the actual connecting once [hintsProvider]
     * replies, which may be on a different thread and some time later.
     */
    @Synchronized
    private fun connect() {
        if (stopped) return
        val config = desiredConfig ?: return
        val provider = hintsProvider ?: return
        provider.invoke { hints -> finishConnect(config, hints) }
    }

    /**
     * Resumes [connect] once the async [hintsProvider] call it kicked off
     * replies with [hints]. Re-checks [stopped] and that [desiredConfig]
     * still matches [config] -- both [stop] and a newer [start] could have
     * landed while the computation was in flight, in which case this reply
     * is stale and must be dropped rather than opening a socket for a target
     * we've already moved on from.
     */
    @Synchronized
    private fun finishConnect(config: RelayConfig, hints: List<ByteArray>) {
        if (!isPushHintReplyCurrent(stopped, desiredConfig, config)) return
        if (hints.isEmpty()) {
            // Nothing addressed to us yet (e.g. no contacts/groups -- the
            // fresh-onboarding state). relayd rejects a hint-less subscribe
            // with 400, so there is no point opening a socket; retry once
            // hints might exist. This is still a failure to connect from the
            // backoff's point of view -- recordFailure() here is what makes
            // that retry back off (to the 60s cap) instead of spinning at
            // the 2s floor forever while onboarding.
            backoff.recordFailure()
            scheduleReconnect()
            return
        }
        val encodedHints = hints.joinToString(",") { urlEncode(base64Url(it)) }
        val wsUrl = "${toWebSocketUrl(config.relayUrl)}/ws?hints=$encodedHints&after=0"
        val request = Request.Builder()
            .url(wsUrl)
            .header("Authorization", "Bearer ${config.relayToken}")
            .build()
        Log.i(TAG, "Connecting relay push socket to ${config.relayUrl}")
        webSocket = httpClient.newWebSocket(request, listener)
    }

    private val listener = object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            Log.i(TAG, "Relay push socket open")
            backoff.recordSuccess()
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            // Content is ignored on purpose -- see class doc. Any pushed
            // envelope, replayed or live, just means "go run the
            // authoritative poll pass now."
            onPush()
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(code, reason)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            Log.i(TAG, "Relay push socket closed: $code $reason")
            handleDisconnect()
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            Log.w(TAG, "Relay push socket failed: ${t.message}")
            handleDisconnect()
        }
    }

    @Synchronized
    private fun handleDisconnect() {
        if (stopped) return
        webSocket = null
        backoff.recordFailure()
        scheduleReconnect()
    }

    @Synchronized
    private fun scheduleReconnect() {
        if (stopped) return
        reconnectRunnable?.let { mainHandler.removeCallbacks(it) }
        val runnable = Runnable { connect() }
        reconnectRunnable = runnable
        mainHandler.postDelayed(runnable, backoff.nextDelayMs())
    }

    private fun toWebSocketUrl(baseUrl: String): String {
        val normalized = normalizeRelayUrl(baseUrl)
        return when {
            normalized.startsWith("https://") -> "wss://${normalized.removePrefix("https://")}"
            normalized.startsWith("http://") -> "ws://${normalized.removePrefix("http://")}"
            else -> normalized
        }
    }

    private fun base64Url(bytes: ByteArray): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    private fun urlEncode(value: String): String =
        URLEncoder.encode(value, StandardCharsets.UTF_8)
}
