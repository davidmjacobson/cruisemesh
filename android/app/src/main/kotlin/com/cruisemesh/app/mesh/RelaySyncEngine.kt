package com.cruisemesh.app.mesh

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Handler
import android.os.SystemClock
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.relay.RelayClient
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import com.cruisemesh.app.relay.RelayHttpException
import com.cruisemesh.app.relay.RelayPushClient
import com.cruisemesh.app.relay.RelayPushSubscription
import com.cruisemesh.app.relay.RelayUpdateSender
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreRelayEnvelopeDisposition
import uniffi.cruisemesh_core.CoreRelayFault
import uniffi.cruisemesh_core.relayClassifyHttpError
import uniffi.cruisemesh_core.relayRetryAfterMs
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.PeerConnectionEventKind
import uniffi.cruisemesh_core.PeerConnectionTransport
import uniffi.cruisemesh_core.coreGroupFanoutRows
import uniffi.cruisemesh_core.dedupeHints
import uniffi.cruisemesh_core.coreGroupFanoutRowsForCarried
import uniffi.cruisemesh_core.recentPresenceHintsFor
import uniffi.cruisemesh_core.relayCursorKey
import uniffi.cruisemesh_core.relayFetchBatchLimit
import uniffi.cruisemesh_core.relayFetchWalkContinues
import uniffi.cruisemesh_core.RelayMailboxWalkAction
import uniffi.cruisemesh_core.relayMailboxContinuationDelayMs
import uniffi.cruisemesh_core.relayMailboxWalkAction
import uniffi.cruisemesh_core.relayPassStartCursor
import uniffi.cruisemesh_core.relaySweepDue
import uniffi.cruisemesh_core.ContactRelayRejection
import uniffi.cruisemesh_core.ContactRelayUnreachable
import uniffi.cruisemesh_core.coreContactRelayEndpointUsable
import uniffi.cruisemesh_core.coreContactRelayIsStale
import uniffi.cruisemesh_core.coreContactRelayStreakDelta
import uniffi.cruisemesh_core.coreContactRelayUnreachableIsStale
import uniffi.cruisemesh_core.coreGroupFanoutRelayTarget
import uniffi.cruisemesh_core.GroupRelayMember
import uniffi.cruisemesh_core.resolvedContactDeliveryPollRelay
import uniffi.cruisemesh_core.resolvedContactDeliveryRelay
import java.util.concurrent.ConcurrentHashMap

// Deliberately MeshService's tag, not this class's name: this code moved here
// verbatim in the FA15 extraction, and field tooling (logcat filters, the
// debug-report scripts) matches on the "MeshService" tag for relay-sync lines.
private const val TAG = "MeshService"

/**
 * FA15: the relay half of what used to be MeshService — everything between
 * "we may have validated internet" and "envelopes moved through the relay":
 * the [ConnectivityManager.requestNetwork] callback and bind-target policy,
 * the poll cadence (including the push-healthy backoff), the
 * [RelayPushClient] subscription, the upload passes (outbound, receipts,
 * family-carried fan-out), mailbox polling with disposition-driven acks, and
 * presence sync.
 *
 * It deliberately knows nothing about envelope *content*: every fetched
 * envelope is handed to [processRelayEnvelope]
 * ([InboundEnvelopeProcessor.handleRelayEnvelope]) and the returned
 * [CoreInboundDisposition] only steers the ack decision. Construction happens
 * in MeshService.onStartCommand once the store and identity exist; state that
 * stays with the service (the `running` flag, the current identity, the
 * Wi‑Fi hold, the FA3 store executor) crosses the seam as injected functions
 * so the threading/visibility semantics are exactly what they were when this
 * was one class.
 */
internal class RelaySyncEngine(
    private val context: Context,
    private val store: MessageStore,
    private val handler: Handler,
    private val connectivityManager: ConnectivityManager,
    private val identityProvider: () -> Identity?,
    private val isRunning: () -> Boolean,
    private val processRelayEnvelope: (RelayFetchedEnvelope, Identity) -> CoreInboundDisposition,
    private val backfillOutgoingReceipts: (Identity, Long) -> Unit,
    private val onRelayNetworkChanged: () -> Unit,
    private val assertOffMainThreadForStore: (String) -> Unit,
    private val runOnStoreExecutorAlwaysReplying: (String, () -> Unit, () -> Unit) -> Unit,
) {

    /**
     * The network relay traffic is pinned to: the best network with validated
     * internet, as granted by [ConnectivityManager.requestNetwork]. The system
     * prefers Wi‑Fi when it is validated and hands us cellular the moment Wi‑Fi
     * stops validating — so this keeps flowing over cellular even while Android
     * still lists an associated-but-dead Wi‑Fi as the system default network.
     * `requestNetwork` (not a passive callback) is required so we are actually
     * permitted to bind sockets to it.
     */
    @Volatile private var relayBindNetwork: Network? = null
    @Volatile private var relaySyncInFlight = false
    @Volatile private var relaySyncPending = false
    private val relaySyncLock = Any()
    private var relayNetworkCallbackRegistered = false

    /** Health [relayPushClient] reported at the last poll-interval decision; null before the first one. See [onRelayPushHealthChanged]/[relayPollRunnable]. */
    @Volatile private var lastKnownPushHealthy: Boolean? = null

    /**
     * Whether the last config sweep found any relay at all, ours or a
     * contact's. Refreshed wherever [distinctRelayConfigs] is computed, and
     * read by [offlineRelayHealth] from callbacks that cannot touch the store.
     * Null until the first sweep runs.
     */
    @Volatile private var anyRelayConfigKnown: Boolean? = null

    /**
     * DTN audit finding F1: the 60s poll is correctness-authoritative but
     * slow. When validated internet is up, this opens relayd's `GET /ws`
     * push socket (relayd/src/lib.rs) and, on every pushed envelope, calls
     * [requestRelaySync] immediately instead of waiting for the next poll
     * tick -- see [updateRelayPushSubscription]. It never processes envelope
     * content itself; see [RelayPushClient]'s class doc.
     *
     * Battery, 2026-07-21: also reports its connection health via
     * [onRelayPushHealthChanged], which [relayPollRunnable] and
     * [scheduleRelayPolling] use (through [RadioPowerPolicy.relayPollIntervalMs])
     * to slow the poll down to a safety net while push is healthy.
     */
    private val relayPushClient = RelayPushClient(
        handler,
        onPush = { requestRelaySync("relay push") },
        onHealthChanged = ::onRelayPushHealthChanged,
    )

    // Backs an INTERNET requestNetwork() (VALIDATED is not requestable, only
    // observable — so we request INTERNET and gate on validation here). The
    // request grants permission to bind to whatever network it assigns, which
    // the framework reassigns from a Wi‑Fi that stops validating to cellular.
    // We only pin traffic to it once it actually reports validated internet.
    private val relayNetworkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
            if (caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)) {
                relayBindNetwork = network
                requestRelaySync("network validated")
            } else if (relayBindNetwork == network) {
                relayBindNetwork = null
            }
            // VPN can come or go under us; keep the Wi‑Fi hold's VPN gating current.
            onRelayNetworkChanged()
            updateRelayPushSubscription()
        }

        override fun onLost(network: Network) {
            if (relayBindNetwork == network) relayBindNetwork = null
            if (!hasValidatedInternet()) {
                MeshConnectivityStatus.setRelayHealth(offlineRelayHealth(anyRelayConfigKnown))
            }
            onRelayNetworkChanged()
            updateRelayPushSubscription()
        }
    }

    /**
     * Battery, 2026-07-21: reposts itself at [RadioPowerPolicy.relayPollIntervalMs]
     * instead of a fixed interval -- see [reschedulePoll]. The poll call
     * itself ([requestRelaySync]) is unchanged and stays
     * correctness-authoritative; only how often it fires changes.
     */
    private val relayPollRunnable = object : Runnable {
        override fun run() {
            requestRelaySync("poll interval")
            reschedulePoll(relayPushClient.isHealthy())
        }
    }

    fun registerRelayNetworkCallback() {
        if (relayNetworkCallbackRegistered) return
        // Ask for an internet-capable network rather than watching only the
        // default. Leaving Wi‑Fi range, Android keeps the dead Wi‑Fi as the
        // default for a while; requestNetwork instead reassigns us to cellular
        // once Wi‑Fi stops validating (and grants permission to bind to it).
        // VALIDATED can't be part of the request, so the callback gates on it.
        val request = NetworkRequest.Builder()
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()
        connectivityManager.requestNetwork(request, relayNetworkCallback)
        relayNetworkCallbackRegistered = true
    }

    fun unregisterRelayNetworkCallback() {
        if (!relayNetworkCallbackRegistered) return
        connectivityManager.unregisterNetworkCallback(relayNetworkCallback)
        relayNetworkCallbackRegistered = false
        relayBindNetwork = null
    }

    /**
     * The network to bind relay sockets to, or null to use the process default.
     *
     * - Default already validated (normal Wi‑Fi/cellular, or an up VPN tunnel):
     *   null — the default works, and binding to a network under a VPN is
     *   forbidden (EPERM) and would bypass the tunnel anyway.
     * - Default missing/unvalidated with no VPN (associated-but-dead Wi‑Fi):
     *   the validated network our [requestNetwork] grant found (cellular), so
     *   relay sync rides it instead of the dead default. This is the fix for
     *   messages not relaying the moment you leave Wi‑Fi.
     * - Default is a VPN that is not (yet) validated: null — respect the
     *   tunnel; we must not bypass it.
     */
    private fun relayBindTarget(): Network? {
        if (isDefaultValidated() || isDefaultVpn()) return null
        return relayBindNetwork
    }

    /** True when a usable validated internet path exists for relay traffic. */
    fun hasValidatedInternet(): Boolean =
        isDefaultValidated() || (!isDefaultVpn() && relayBindNetwork != null)

    /** FA3: runs on the store executor -- see the call sites in MeshService.onStartCommand. */
    fun publishInitialRelayHealth() {
        assertOffMainThreadForStore("publishInitialRelayHealth")
        val contacts = try {
            store.listContacts()
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to inspect contacts for initial relay status: ${e.message}")
            emptyList()
        }
        val configs = distinctRelayConfigs(contacts, RelayConfigStore.load(context))
        anyRelayConfigKnown = configs.isNotEmpty()
        MeshConnectivityStatus.setRelayHealth(
            when {
                configs.isEmpty() -> RelayHealth.NoConfig
                !hasValidatedInternet() -> RelayHealth.NoInternet
                else -> RelayHealth.Checking
            },
        )
    }

    private fun defaultCaps(): NetworkCapabilities? =
        connectivityManager.activeNetwork?.let { connectivityManager.getNetworkCapabilities(it) }

    private fun isDefaultValidated(): Boolean {
        val caps = defaultCaps() ?: return false
        return caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
            caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
    }

    /** A VPN owns the app's default route (so we must not bind past it). */
    fun isDefaultVpn(): Boolean {
        val caps = defaultCaps() ?: return false
        return !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
    }

    /** Short transport label for a network, for relay-sync diagnostics. */
    private fun networkLabel(network: Network?): String {
        if (network == null) return "none"
        val caps = connectivityManager.getNetworkCapabilities(network) ?: return "unknown"
        return when {
            caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN) -> "vpn"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> "wifi"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> "cellular"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> "ethernet"
            else -> "other"
        }
    }

    fun scheduleRelayPolling() {
        // Push health is unknown at this point (RelayPushClient hasn't been
        // (re)started for this session yet) -- start at the unhealthy/safety
        // cadence; the first real health report reschedules from there via
        // [onRelayPushHealthChanged].
        lastKnownPushHealthy = null
        handler.removeCallbacks(relayPollRunnable)
        handler.postDelayed(relayPollRunnable, RadioPowerPolicy.RELAY_POLL_UNHEALTHY_MS)
    }

    fun cancelRelayPolling() {
        handler.removeCallbacks(relayPollRunnable)
        handler.removeCallbacks(rateLimitRetryRunnable)
        handler.removeCallbacks(mailboxContinuationRunnable)
    }

    /** Stops the push socket; MeshService.onDestroy's counterpart to [updateRelayPushSubscription]. */
    fun stopPush() {
        relayPushClient.stop()
    }

    /**
     * Recomputes the poll interval from [RadioPowerPolicy.relayPollIntervalMs]
     * given [currentlyHealthy] and re-arms [relayPollRunnable] with it,
     * cancelling whatever was previously scheduled. Called both from
     * [relayPollRunnable] itself (every tick decides its own next interval)
     * and from [onRelayPushHealthChanged] (so a health transition reschedules
     * immediately rather than waiting out whatever long interval is already
     * pending).
     */
    private fun reschedulePoll(currentlyHealthy: Boolean) {
        val interval = RadioPowerPolicy.relayPollIntervalMs(lastKnownPushHealthy, currentlyHealthy)
        lastKnownPushHealthy = currentlyHealthy
        handler.removeCallbacks(relayPollRunnable)
        handler.postDelayed(relayPollRunnable, interval)
    }

    /** [RelayPushClient]'s health-change callback -- see [relayPushClient]'s doc and [RadioPowerPolicy]'s "Relay poll cadence" section. */
    private fun onRelayPushHealthChanged(healthy: Boolean) {
        Log.i(TAG, "Relay push health -> $healthy")
        reschedulePoll(healthy)
        // Mirrors the signal for the Compose layer -- see MeshConnectivityStatus.pushHealthy
        // and ContactReachability.selfRelayHealthy's pushHealthy param: without this, the
        // "Online via relay" badge and the relay-health pill would falsely degrade after
        // ~120-150s of push-healthy-but-quiet, since the poll (which used to be the only
        // thing refreshing RelayHealth.Ok's lastSyncMs) now backs off to 900s while push is up.
        MeshConnectivityStatus.setPushHealthy(healthy)
    }

    /**
     * Starts [relayPushClient] against our own relay config once validated
     * internet and an identity exist, or stops it otherwise (no config, no
     * identity yet, or the network went away). Called on service start and
     * on every relay network capability change, mirroring how
     * [requestRelaySync] is triggered from the same places -- the push
     * socket should be up in exactly the situations the poll would already
     * succeed in.
     *
     * The hint set passed to [RelayPushClient.start] is recomputed on every
     * (re)connect from [MessageStore.relayFetchPushHints] (mail addressed to
     * us, plus mail addressed to a contact we can proxy-fetch for, same ids
     * as [pollRelayMailbox]'s [MessageStore.relayFetchHints] doc, plus one
     * day ahead -- see that function's doc for why a push *subscription*
     * safely reaches a day further than a fetch) so a
     * newly added contact or group is picked up the next reconnect without
     * this needing its own change-tracking; until then the 60s poll already
     * covers it.
     *
     * FA3: that recomputation used to run synchronously on the main thread
     * inside [RelayPushClient.connect] (whichever thread called [start] or
     * its own delayed reconnect, always the relay handler's looper). It's
     * now an async callback -- [RelayPushClient] hands us a completion
     * function instead of expecting a return value, we compute the hints on
     * the store executor via [computeRelayPushHints], and it resumes
     * connecting once we call back. See [RelayPushClient]'s class doc for the
     * resulting state machine.
     */
    fun updateRelayPushSubscription() {
        val identity = identityProvider()
        val config = RelayConfigStore.load(context)
        if (identity == null || config == null || !hasValidatedInternet()) {
            relayPushClient.stop()
            return
        }
        relayPushClient.start(config) { onReady -> computeRelayPushHints(identity, config, onReady) }
    }

    /**
     * FA3: computes the relay-push subscription on the store executor and
     * always invokes [onReady] exactly once -- with the hints, with an empty
     * hint set if the computation throws, or with an empty hint set if the
     * executor has already been shut down (MeshService.onDestroy racing a
     * pending reconnect). [RelayPushClient.connect] depends on hearing back
     * to decide whether to open a socket or back off and retry (empty hints
     * reads as "nothing to subscribe to yet," same as before this fix);
     * silently dropping the callback would strand it never reconnecting.
     *
     * Two things go into a subscription, and both matter:
     *
     *  - **Which hints**, from [MessageStore.relayFetchPushHints] -- the
     *    fetch id set plus one day ahead, so a socket opened before midnight
     *    is still subscribed to the right topic after it (see that function's
     *    doc for why a subscription may safely reach a day further than a
     *    fetch).
     *  - **Where to replay from**: the poll path's persisted frontier for
     *    this relay, so a reconnect asks relayd for what arrived since rather
     *    than for the whole mailbox. The doorbell ignores frame content
     *    either way -- this is purely about not making the server serialize
     *    an entire mailbox into frames that are discarded on arrival, on
     *    every reconnect, forever.
     */
    private fun computeRelayPushHints(
        identity: Identity,
        config: RelayConfig,
        onReady: (RelayPushSubscription) -> Unit,
    ) {
        val empty = RelayPushSubscription(emptyList(), 0L)
        runOnStoreExecutorAlwaysReplying("relay push hint computation", { onReady(empty) }) {
            assertOffMainThreadForStore("relay push hint computation")
            val now = System.currentTimeMillis()
            val computed = try {
                val hints = store.relayFetchPushHints(identity.userId, now)
                val cursorKey = relayCursorKey(config.relayUrl, config.relayToken)
                RelayPushSubscription(hints, store.relayFetchCursor(cursorKey).afterId)
            } catch (e: CoreException) {
                Log.w(TAG, "Failed to compute relay push hints: ${e.message}")
                empty
            }
            onReady(computed)
        }
    }

    fun requestRelaySync(reason: String) {
        if (!isRunning() || identityProvider() == null) return
        if (!hasValidatedInternet()) {
            MeshConnectivityStatus.setRelayHealth(offlineRelayHealth(anyRelayConfigKnown))
            return
        }
        // CP2b: honor relayd's Retry-After. Every nudge that arrives inside
        // the advertised window (poll tick, push frame, queue change)
        // coalesces into one retry at the window's end instead of hammering
        // a service that just said "too fast".
        val backoffRemainingMs = rateLimitedUntilMs - System.currentTimeMillis()
        if (backoffRemainingMs > 0) {
            handler.removeCallbacks(rateLimitRetryRunnable)
            handler.postDelayed(rateLimitRetryRunnable, backoffRemainingMs)
            return
        }
        synchronized(relaySyncLock) {
            if (relaySyncInFlight) {
                relaySyncPending = true
                return
            }
            relaySyncInFlight = true
        }
        Thread {
            while (true) {
                try {
                    performRelaySyncPass(reason)
                } catch (e: FamilyRateLimitAbort) {
                    val remainingMs = (rateLimitedUntilMs - System.currentTimeMillis()).coerceAtLeast(1L)
                    Log.w(TAG, "Family relay rate limit halted sync pass ($reason); retrying in ${remainingMs}ms")
                    MeshConnectivityStatus.setRelayHealth(RelayHealth.RateLimited(System.currentTimeMillis()))
                    handler.removeCallbacks(rateLimitRetryRunnable)
                    handler.postDelayed(rateLimitRetryRunnable, remainingMs)
                } catch (e: Exception) {
                    Log.w(TAG, "Relay sync failed ($reason): ${e.message}")
                    MeshConnectivityStatus.setRelayHealth(RelayHealth.Failing(System.currentTimeMillis()))
                }
                val rerun = synchronized(relaySyncLock) {
                    val action = relayRerunAction(
                        pendingRequested = relaySyncPending,
                        canSync = isRunning() && hasValidatedInternet(),
                        backoffRemainingMs = rateLimitedUntilMs - System.currentTimeMillis(),
                    )
                    relaySyncPending = false
                    when (action) {
                        RelayRerunAction.RUN_AGAIN -> true
                        RelayRerunAction.SCHEDULE_RATE_LIMIT_RETRY -> {
                            // See relayRerunAction's KDoc: the pending nudge
                            // coalesces into the same Retry-After timer the
                            // front door uses, instead of re-running into a
                            // relay that just said "too fast".
                            handler.removeCallbacks(rateLimitRetryRunnable)
                            handler.postDelayed(
                                rateLimitRetryRunnable,
                                rateLimitedUntilMs - System.currentTimeMillis(),
                            )
                            relaySyncInFlight = false
                            false
                        }
                        RelayRerunAction.STOP -> {
                            relaySyncInFlight = false
                            false
                        }
                    }
                }
                if (!rerun) break
            }
        }.start()
    }

    private fun performRelaySyncPass(reason: String) {
        val identity = identityProvider() ?: return
        val now = System.currentTimeMillis()
        familyBackoffIdentityHash = identity.userId.contentHashCode()
        mailboxContinuationNeeded = false
        store.pruneExpiredOutboundEnvelopes(now)
        store.pruneExpiredOutgoingReceiptEnvelopes(now)
        store.pruneExpiredCarried(now)
        // Same expiry-driven family: once an envelope is expired its relay
        // copy is ackable on the EXPIRED disposition alone, so the record that
        // this device consumed it has nothing left to prove.
        store.pruneExpiredConsumedHiddenMsgIds(now)
        val contacts = store.listContacts()
        val fallbackConfig = RelayConfigStore.load(context)
        // Bind this whole pass to a validated network when the default can't be
        // trusted (associated-but-dead Wi‑Fi, no VPN); otherwise null = use the
        // default (normal networks and VPN tunnels route themselves).
        val network = relayBindTarget()
        ownRelayFault = null
        passNowMs = now
        contactRelayRejections = store.listContactRelayRejections()
            .associateByTo(mutableMapOf()) { UserIdHex.encode(it.userId) }
        contactRelayUnreachable = store.listContactRelayUnreachable()
            .associateByTo(mutableMapOf()) { UserIdHex.encode(it.userId) }
        contactRelayCountedThisPass.clear()
        contactRelaySilence.restore(contactRelayUnreachable.values)
        contactRelaySilence.beginPass()
        publishStaleContactRelays()
        backfillOutgoingReceipts(identity, now)
        // T23: if our own endpoint changed since the last announcement, queue
        // the notice to every contact *before* this pass uploads, so it rides
        // out in the same sync. This is the single trigger for every way the
        // config can change (Shore Pass setup and removal, manual entry in
        // Advanced, a scanned setup card, a backup restore) because they all
        // already end in `RelaySyncEvents.requestSync()` — no save site has to
        // remember to announce, and none can be missed. `announceIfChanged` is
        // idempotent, so the periodic poll re-entering here costs nothing.
        RelayUpdateSender.announceIfChanged(context, store, identity)
        uploadPendingOutgoingReceiptEnvelopes(contacts, fallbackConfig, now, network)
        uploadPendingOutboundEnvelopes(contacts, fallbackConfig, now, network)
        uploadFamilyCarriedEnvelopes(contacts, fallbackConfig, now, network)

        // Gaining a contact or a group widens the fetch-hint set, and relayd's
        // next_cursor only ever covers the hints we sent -- so mail that
        // arrived under a hint we did not have yet is already *below* the
        // frontier, where no sweep interval can reach it. Core notices the
        // change and drops the frontiers; the walks below then start at 0.
        // Cheap when nothing changed (one digest of the id set, no rows
        // touched), so it is safe to run every pass.
        runCatching { store.noteRelayHintSources(identity.userId) }
            .onSuccess { rewalking ->
                if (rewalking) {
                    Log.i(TAG, "Hint sources changed; re-walking every relay mailbox from the start")
                }
            }
            .onFailure { error ->
                Log.w(TAG, "Could not check relay hint sources: ${error.message}")
            }

        val configs = distinctRelayConfigs(contacts, fallbackConfig)
        anyRelayConfigKnown = configs.isNotEmpty()
        if (configs.isEmpty()) {
            MeshConnectivityStatus.setRelayHealth(RelayHealth.NoConfig)
            return
        }
        var anyRelaySucceeded = false
        var ownRelaySucceeded = fallbackConfig == null
        // Distinct from ownRelaySucceeded, which starts true when there is no
        // own relay at all and can also be satisfied by a pass that issued no
        // request. Only a mailbox that actually answered counts as proof this
        // device has working internet, so only that may license resting a
        // contact's silent endpoint.
        var ownRelayAnswered = false
        for (config in configs) {
            try {
                val answered = pollRelayMailbox(config, identity, now, network)
                syncRelayPresence(config, identity, contacts, fallbackConfig, now, network)
                anyRelaySucceeded = true
                if (config == fallbackConfig) {
                    ownRelaySucceeded = true
                    if (answered) ownRelayAnswered = true
                }
            } catch (e: Exception) {
                rethrowFamilyRateLimit(e)
                // A contact can carry stale relay credentials from an older
                // friend card. That relay failing must not abort polling of
                // the remaining relays or declare our own configured relay
                // unreachable when it succeeded.
                noteOwnRelayFault(config, fallbackConfig, e)
                Log.w(TAG, "Relay sync failed for ${config.relayUrl}: ${e.message}")
            }
        }
        // T11 + CP2b: structured rejections of our OWN saved config beat both
        // the generic Failing state and (for the mailbox-level faults) a
        // successful poll -- see relayHealthAfterSyncPass's KDoc.
        MeshConnectivityStatus.setRelayHealth(
            relayHealthAfterSyncPass(ownRelayFault, ownRelaySucceeded, anyRelaySucceeded, now),
        )
        // Reaching the end means every request completed without a new 429.
        rateLimitedUntilMs = 0L
        familyRelayBackoff.onSuccessfulPass()
        // Now that the pass knows whether our own mailbox answered, this
        // pass's silent contact endpoints can be judged (or discarded).
        commitUnreachableContactRelays(ownRelayAnswered, contacts)
        // Re-read: the uploads above may have advanced or cleared streaks, and
        // a person who just watched a message fail should not have to wait a
        // whole poll interval for the explanation to appear.
        contactRelayRejections = store.listContactRelayRejections()
            .associateByTo(mutableMapOf()) { UserIdHex.encode(it.userId) }
        contactRelayUnreachable = store.listContactRelayUnreachable()
            .associateByTo(mutableMapOf()) { UserIdHex.encode(it.userId) }
        publishStaleContactRelays()
        val netDesc = if (network != null) "${networkLabel(network)}(pinned)" else "${networkLabel(connectivityManager.activeNetwork)}(default)"
        Log.i(TAG, "Relay sync complete: configs=${configs.size} net=$netDesc reason=$reason")
        if (mailboxContinuationNeeded) scheduleMailboxContinuation()
    }

    /**
     * Recipients we already know we cannot post to on this pass.
     *
     * Core excludes these in the query rather than the upload loops skipping
     * them afterwards: a row that is fetched and then skipped has still
     * consumed one of [RELAY_STORE_BATCH_LIMIT] slots, so filtering downstream
     * leaves the starvation completely intact -- the app can diagnose a dead
     * contact and still be unable to act on it.
     */
    private fun unpostableRecipients(
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
    ): List<ByteArray> = contacts
        .filter { resolvedRelayConfig(it, fallbackConfig) == null }
        .map { it.userId }

    private fun uploadPendingOutgoingReceiptEnvelopes(
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
        now: Long,
        network: Network?,
    ) {
        val contactsByUserId = contacts.associateBy { UserIdHex.encode(it.userId) }
        val skipRecipients = unpostableRecipients(contacts, fallbackConfig)
        for (envelope in store.pendingRelayOutgoingReceiptEnvelopes(RELAY_STORE_BATCH_LIMIT, now, skipRecipients)) {
            val contact = contactsByUserId[UserIdHex.encode(envelope.recipientUserId)] ?: continue
            val config = resolvedRelayConfig(contact, fallbackConfig) ?: continue
            try {
                val relayId = relayRequest { RelayClient.postReceiptEnvelope(config, envelope, network) }
                store.markOutgoingReceiptEnvelopeRelayPosted(envelope.msgId, now)
                noteContactRelaySuccess(contact, config, fallbackConfig)
                Log.i(
                    TAG,
                    "Uploaded receipt envelope ${UserIdHex.encode(envelope.msgId)} to relay ${config.relayUrl} as id=$relayId",
                )
            } catch (e: Exception) {
                rethrowFamilyRateLimit(e)
                noteOwnRelayFault(config, fallbackConfig, e)
                noteContactRelayFault(contact, config, fallbackConfig, e)
                Log.w(TAG, "Failed to upload receipt envelope to relay ${config.relayUrl}: ${e.message}")
            }
        }
    }

    private fun uploadPendingOutboundEnvelopes(
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
        now: Long,
        network: Network?,
    ) {
        val contactsByUserId = contacts.associateBy { UserIdHex.encode(it.userId) }
        val skipRecipients = unpostableRecipients(contacts, fallbackConfig)
        if (skipRecipients.isNotEmpty()) {
            Log.i(
                TAG,
                "Skipping relay upload for ${skipRecipients.size} unreachable recipient(s) this pass: " +
                    skipRecipients.joinToString { UserIdHex.encode(it) },
            )
        }
        // A stranded outbound queue was previously invisible in a support
        // archive: "nothing is arriving" read the same whether the queue was
        // deep or empty. One lopsided recipient here is the signature of a
        // contact whose relay is unreachable.
        val queueDepth = store.pendingRelayOutboundDepthByRecipient(now)
        if (queueDepth.isNotEmpty()) {
            Log.i(
                TAG,
                "Outbound relay queue depth by recipient: " +
                    queueDepth.joinToString { "${UserIdHex.encode(it.recipientUserId)}=${it.queued}" },
            )
        }
        for (envelope in store.pendingRelayOutboundEnvelopes(RELAY_STORE_BATCH_LIMIT, now, skipRecipients)) {
            // 1:1 / invite envelopes are addressed to a contact userId; group
            // text uses recipientUserId = group.id and rides the family's
            // fallback (or any member's) relay config.
            val contact = contactsByUserId[UserIdHex.encode(envelope.recipientUserId)]
            if (contact == null) {
                // Group-addressed: per-member fan-out instead of one shared
                // group-hint row (specs/group-relay-durability.md §4.2).
                val group = store.getGroup(envelope.recipientUserId)
                val config = relayConfigForGroupRecipient(envelope.recipientUserId, contacts, fallbackConfig)
                    ?: continue
                if (group == null) {
                    // Recipient is neither contact nor imported group (e.g. a
                    // group deleted mid-queue); keep the legacy single post so
                    // the envelope isn't stranded.
                    try {
                        relayRequest { RelayClient.postOutboundEnvelope(config, envelope, network) }
                        store.markOutboundEnvelopeRelayPosted(envelope.msgId, now)
                    } catch (e: Exception) {
                        rethrowFamilyRateLimit(e)
                        noteOwnRelayFault(config, fallbackConfig, e)
                        Log.w(TAG, "Failed to upload outbound envelope to relay ${config.relayUrl}: ${e.message}")
                    }
                    continue
                }
                val rows = coreGroupFanoutRows(
                    envelope.msgId,
                    group.memberUserIds,
                    envelope.hopTtl,
                    envelope.expiry,
                    envelope.sealed,
                    envelope.timestamp,
                )
                // Spec §4.2: mark relay-posted only after ALL member rows
                // post. A partial failure retries the whole set next pass;
                // the deterministic fan-out msg_ids dedupe server-side.
                var posted = 0
                for (row in rows) {
                    try {
                        relayRequest { RelayClient.postFanoutRow(config, row, network) }
                        posted++
                    } catch (e: Exception) {
                        rethrowFamilyRateLimit(e)
                        noteOwnRelayFault(config, fallbackConfig, e)
                        Log.w(TAG, "Failed to upload fan-out row to relay ${config.relayUrl}: ${e.message}")
                    }
                }
                if (posted == rows.size) {
                    store.markOutboundEnvelopeRelayPosted(envelope.msgId, now)
                    Log.i(
                        TAG,
                        "Uploaded group envelope ${UserIdHex.encode(envelope.msgId)} as $posted fan-out row(s) to relay ${config.relayUrl}",
                    )
                }
                continue
            }
            val config = resolvedRelayConfig(contact, fallbackConfig) ?: continue
            try {
                val relayId = relayRequest { RelayClient.postOutboundEnvelope(config, envelope, network) }
                store.markOutboundEnvelopeRelayPosted(envelope.msgId, now)
                noteContactRelaySuccess(contact, config, fallbackConfig)
                Log.i(
                    TAG,
                    "Uploaded outbound envelope ${UserIdHex.encode(envelope.msgId)} to relay ${config.relayUrl} as id=$relayId",
                )
            } catch (e: Exception) {
                rethrowFamilyRateLimit(e)
                noteOwnRelayFault(config, fallbackConfig, e)
                noteContactRelayFault(contact, config, fallbackConfig, e)
                Log.w(TAG, "Failed to upload outbound envelope to relay ${config.relayUrl}: ${e.message}")
            }
        }
    }

    /**
     * Which single mailbox a group envelope's fan-out rows go to, or null for
     * "post nothing this pass" -- which leaves the envelope queued for a later
     * pass and for the BLE/LAN paths, exactly as the 1:1 skip does.
     *
     * The choice itself is core's ([coreGroupFanoutRelayTarget]) because the
     * rule that matters is easy to get subtly wrong in one shell: a member
     * whose endpoint is *resting for silence* contributes no fallback to our
     * own mailbox. Falling back for them would post a cross-family member's
     * copy where they never read, and `relay_posted_at` is terminal, so that
     * is a permanent misroute rather than a retry. A member written off for
     * *rejection* still falls back, unchanged.
     */
    private fun relayConfigForGroupRecipient(
        groupId: ByteArray,
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
    ): RelayConfig? {
        val group = store.getGroup(groupId) ?: return fallbackConfig
        val members = group.memberUserIds.mapNotNull { memberId ->
            val contact = contacts.firstOrNull { it.userId.contentEquals(memberId) }
                ?: return@mapNotNull null
            GroupRelayMember(
                contact.relayUrl,
                contact.relayToken,
                contactEndpointUsable(contact),
                contactEndpointAnswering(contact),
            )
        }
        return coreGroupFanoutRelayTarget(
            members,
            fallbackConfig?.relayUrl,
            fallbackConfig?.relayToken,
        )?.let { RelayConfig(it.url, it.token) }
    }

    private fun uploadFamilyCarriedEnvelopes(
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
        now: Long,
        network: Network?,
    ) {
        // Carried mail starves like the outbound and receipt queues: a failed
        // upload leaves the row unmarked, so under flat order one unreachable
        // destination refills the batch every pass. Core resolves each row's
        // rotating recipient hint to a contact so it can partition and skip.
        val skipRecipients = unpostableRecipients(contacts, fallbackConfig)
        for (envelope in store.familyCarriedEnvelopes(RELAY_STORE_BATCH_LIMIT, now, skipRecipients)) {
            val contact = store.contactMatchingHint(envelope.recipientHint, now)
            if (contact == null) {
                // Group-hinted carried envelope: previously skipped entirely
                // (no contact match). A member mule can now decompose it into
                // per-member fan-out rows (specs/group-relay-durability.md
                // §4.2) so the group's mail reaches internet-only members
                // through this phone's uplink too. Non-member mules still
                // can't recognize the hint and still skip, unchanged. The
                // envelope is stamped uploaded only once EVERY fan-out row
                // landed -- a partial batch re-posts whole next pass, and the
                // deterministic fan-out ids dedupe the rows that did land.
                val group = store.groupMatchingHint(envelope.recipientHint, now) ?: continue
                val config = relayConfigForGroupRecipient(group.id, contacts, fallbackConfig) ?: continue
                val rows = coreGroupFanoutRowsForCarried(
                    envelope.msgId,
                    group.memberUserIds,
                    envelope.hopTtl,
                    envelope.expiry,
                    envelope.sealed,
                )
                var posted = 0
                for (row in rows) {
                    try {
                        relayRequest { RelayClient.postFanoutRow(config, row, network) }
                        posted++
                    } catch (e: Exception) {
                        rethrowFamilyRateLimit(e)
                        noteOwnRelayFault(config, fallbackConfig, e)
                        Log.w(TAG, "Failed to upload carried fan-out row to relay ${config.relayUrl}: ${e.message}")
                    }
                }
                if (posted == rows.size && rows.isNotEmpty()) {
                    store.markCarriedEnvelopeRelayUploaded(envelope.msgId, config.relayUrl)
                }
                continue
            }
            val config = resolvedRelayConfig(contact, fallbackConfig) ?: continue
            try {
                val relayId = relayRequest { RelayClient.postCarriedEnvelope(config, envelope, network) }
                noteContactRelaySuccess(contact, config, fallbackConfig)
                // 2xx: the relay holds it now (a dedupe hit counts -- the
                // response id proves presence either way). Stamp the row so
                // the next pass offers the NEXT batch instead of re-posting
                // this one forever; see markCarriedEnvelopeRelayUploaded.
                store.markCarriedEnvelopeRelayUploaded(envelope.msgId, config.relayUrl)
                Log.i(
                    TAG,
                    "Uploaded carried envelope ${UserIdHex.encode(envelope.msgId)} to relay ${config.relayUrl} as id=$relayId",
                )
            } catch (e: Exception) {
                rethrowFamilyRateLimit(e)
                noteOwnRelayFault(config, fallbackConfig, e)
                // Parity with the other two upload loops and with
                // MeshController.swift, which already counted this path: a
                // mule posting to a contact's endpoint is exactly as good a
                // witness to that endpoint's health as a sender is.
                noteContactRelayFault(contact, config, fallbackConfig, e)
                Log.w(TAG, "Failed to upload carried envelope to relay ${config.relayUrl}: ${e.message}")
            }
        }
    }

    /**
     * Fetches this config's relay mailbox and, per [CoreInboundDisposition],
     * either consumes each envelope for good or leaves it be.
     *
     * The fetch itself covers two disjoint concerns, combined into one hint
     * set so they ride the same paginated fetch:
     * [MessageStore.relaySelfHints] (mail addressed to us, pairwise or
     * via a group we belong to) and [MessageStore.relayProxyHints] (mail
     * addressed to a *contact*, fetched on their behalf -- relay
     * proxy-polling, see that function's doc for why this is the fix for "a
     * 1:1 message to a WiFi-less recipient never bridges across BLE
     * clusters"). Every fetched envelope still goes through
     * [processRelayEnvelope] -> [InboundEnvelopeProcessor.processInboundEnvelope]
     * exactly as before; what's new is that the ack decision now follows the
     * returned [CoreInboundDisposition] via
     * [MessageStore.coreRelayAckIdsWithConsumed] instead of unconditionally
     * acking everything the fetch returned. A proxied envelope comes back as
     * CARRIED, not CONSUMED, so it is deliberately left on the relay --
     * [InboundEnvelopeProcessor.carryRelayEnvelope] already queued it for BLE
     * delivery to its real recipient, and the relay copy remains the durable
     * fallback until they (or another proxy) fetch and consume it, or it
     * expires. A SEEN envelope this device already consumed as a 1:1 message
     * over BLE/LAN is also acked (DTN_TODOS.md §3.1) instead of being
     * re-fetched on every pass until expiry -- including receipts and the
     * other service kinds that leave no `messages` row, on the strength of
     * the consumed-set [InboundEnvelopeProcessor] records under the same
     * sole-endpoint-consumer rule. See [CoreRelayEnvelopeDisposition]'s KDoc
     * and [MessageStore.coreRecordConsumedHiddenMsgId] for the exact rule.
     *
     * ### Where the walk starts (the persistent frontier)
     *
     * This used to start every pass at `after = 0` and page forward to the
     * end. The un-acked rows above are left on the relay *by design*, so a
     * real mailbox only grows, relayd returns rows in ascending id order,
     * and a **fresh** message therefore has the highest id and was fetched
     * last -- after every stale row ahead of it. In the field that reached
     * ~29k rows at 16 rows a page: thousands of sequential HTTP round trips
     * before the newest message was looked at, and passes that regularly died
     * on a timeout before finishing. Messages took minutes to arrive.
     *
     * A pass now resumes from the frontier persisted for this mailbox
     * ([MessageStore.relayFetchCursor], keyed by [relayCursorKey]) and
     * advances it per [MessageStore.advanceRelayFetchCursor] -- which never
     * moves past a page that did not reach a terminal disposition for every
     * envelope *and* land its acks, and never moves backwards. That is the
     * mirror of the DTN ack-safety rule applied to skipping: an envelope
     * whose processing threw must be re-presented next pass, so nothing may
     * be persisted past it.
     *
     * A pass is also bounded by [relayMailboxWalkAction], whose budget lives
     * in the core so iOS bounds its walk with the same numbers. This matters
     * when a legacy/current backup restores without `relay_fetch_cursors`:
     * starting at zero is correct, but must not synchronously drain an
     * arbitrarily deep mailbox. Safe pages advance the durable cursors, then
     * the walk yields and schedules a delayed continuation from that point.
     *
     * Occasionally the pass sweeps instead -- walks the whole mailbox rather
     * than only what is new -- so those deliberately-unacked rows stay
     * re-discoverable for the phones that depend on this one re-offering
     * them over Bluetooth, and so a relay rebuilt with its row ids restarted
     * at 1 heals itself. [relaySweepDue] owns when, from the *persisted*
     * sweep timestamp: every [uniffi.cruisemesh_core.relaySweepIntervalMs],
     * plus the first pass against a mailbox never swept at all. Notably NOT
     * every process start -- this service is killed and restarted all day, a
     * sweep re-downloads the sealed body of every row still in the mailbox,
     * and tying that to the restart rate made the interval meaningless.
     *
     * A sweep also carries its own resume cursor
     * ([RelayFetchCursor.sweepAfterId], advanced through
     * [MessageStore.advanceRelaySweepCursor] under the frontier's rule) and
     * resumes from it rather than restarting at 0. It has to: the budget above
     * hands a deep mailbox back after four pages, and a sweep is only recorded
     * complete on the empty page at the end of it. Restarting at 0 on every
     * continuation meant any mailbox holding more than one budget's worth of
     * hint-matching rows re-downloaded the same first pages every second or
     * so, indefinitely, and never finished a sweep at all. The frontier cannot
     * stand in for that cursor -- it never moves backwards, so on a
     * long-established mailbox it says nothing about where the sweep is.
     *
     * TODO(relay-proxy-polling follow-up): [MessageStore.relayProxyHints]
     * fetches every contact's hints on every pass, so its cost scales with
     * contact-list size. Fine for this app's small family circles; would need
     * a smarter server-side "for this family token" fan-out if that ever
     * became a large flat social graph.
     */
    private fun pollRelayMailbox(
        config: RelayConfig,
        identity: Identity,
        now: Long,
        network: Network?,
    ): Boolean {
        val fetchHints = store.relayFetchHints(identity.userId, now)
        if (fetchHints.isEmpty()) return false
        val cursorKey = relayCursorKey(config.relayUrl, config.relayToken)
        val cursor = store.relayFetchCursor(cursorKey)
        val sweeping = relaySweepDue(
            sweptThisSession.contains(cursorKey),
            cursor.lastSweepAtMs,
            cursor.sweepAfterId,
            now,
        )
        var after = relayPassStartCursor(sweeping, cursor.afterId, cursor.sweepAfterId)
        // Once any page fails to fully process, both cursors stop moving for
        // the rest of this pass -- persisting a later page's cursor would
        // skip the failed one forever. The walk itself continues, so one bad
        // envelope never blocks the mail behind it.
        var cursorsAdvancing = true
        // Not a val: a page this client cannot take halves the ask and retries
        // the same cursor, and the reduced limit is kept for the rest of this
        // mailbox's walk rather than reset per page -- a mailbox that produced
        // one oversize window usually produces the next one too, and
        // rediscovering that costs a wasted request every page. It is a local
        // of this function and so scoped to THIS mailbox, exactly as in
        // MeshController.swift: one relay's oversize page says nothing about
        // the next relay's, and carrying the reduction across configs would
        // shrink every other mailbox's pages too. The next pass starts from
        // the full limit again.
        var fetchBatchLimit = relayFetchBatchLimit().toInt()
        Log.i(
            TAG,
            "Relay mailbox walk on ${config.relayUrl}: ${if (sweeping) "sweep" else "frontier"} from after=$after",
        )
        // Set the moment a page comes back: the caller uses this as proof that
        // this device's internet works, so it must mean "this mailbox
        // answered", not "the walk was attempted".
        var answered = false
        var pagesFetched = 0u
        var envelopesFetched = 0u
        while (isRunning() && hasValidatedInternet()) {
            val fetched = relayRequest {
                RelayClient.fetchEnvelopesWithinResponseCap(
                    config,
                    fetchHints,
                    after,
                    fetchBatchLimit,
                    network,
                ) { tried, smaller ->
                    Log.w(
                        TAG,
                        "Relay ${config.relayUrl} page after=$after was too big to take at limit=$tried; " +
                            "retrying with limit=$smaller",
                    )
                }
            }
            val page = fetched.page
            fetchBatchLimit = fetched.limit
            answered = true
            Log.i(
                TAG,
                "Fetched ${page.envelopes.size} relay envelope(s) from ${config.relayUrl} after=$after next=${page.nextCursor}",
            )
            if (page.envelopes.isEmpty()) {
                if (sweeping) noteSweepCompleted(cursorKey, now)
                return true
            }
            pagesFetched += 1u
            envelopesFetched += page.envelopes.size.toUInt()
            var pageFullyProcessed = true
            val dispositions = ArrayList<CoreRelayEnvelopeDisposition>(page.envelopes.size)
            for (envelope in page.envelopes) {
                val disposition = try {
                    processRelayEnvelope(envelope, identity)
                } catch (e: Exception) {
                    // Terminal for this page's cursor purposes only in the
                    // negative sense: we do NOT know what happened to this
                    // envelope, so the frontier must not pass it.
                    pageFullyProcessed = false
                    Log.w(
                        TAG,
                        "Failed to process relay envelope id=${envelope.id} from ${config.relayUrl}: ${e.message}",
                    )
                    continue
                }
                dispositions += CoreRelayEnvelopeDisposition(
                    relayId = envelope.id,
                    msgId = envelope.msgId,
                    disposition = disposition,
                    recipientHint = envelope.recipientHint,
                )
                // A contact-hinted envelope coming out of THIS mailbox is
                // proof the mailbox its recipient polls already holds it
                // (proxy-poll parity: a contact's hints are only ever fetched
                // against that contact's resolved relay). If we also carry
                // the same msg_id from a BLE/LAN encounter, stamp that row so
                // the upload loop stops re-posting a copy the relay
                // demonstrably has (no-op when we carry nothing). Group-hinted
                // rows are deliberately NOT stamped here: one mailbox holding
                // a legacy shared row says nothing about the other members'
                // mailboxes the fan-out still owes -- they are stamped only by
                // a complete fan-out post above. Bookkeeping only: a failure
                // here must not fail the walk.
                try {
                    if (store.contactMatchingHint(envelope.recipientHint, now) != null) {
                        store.markCarriedEnvelopeRelayUploaded(envelope.msgId, config.relayUrl)
                    }
                } catch (e: CoreException) {
                    Log.w(TAG, "Failed to stamp fetched envelope as relay-held: ${e.message}")
                }
            }
            // Consumed/Expired ack unconditionally; a SEEN envelope is
            // acked only if this device durably consumed it as a 1:1
            // message from someone else (DTN_TODOS.md §3.1); a legacy
            // shared-mailbox group-hint row is never acked at all
            // (specs/group-relay-durability.md §5.2) -- see
            // CoreRelayEnvelopeDisposition's KDoc.
            val ackIds = store.coreRelayAckIdsWithConsumed(dispositions, identity.userId, now)
            if (ackIds.isNotEmpty()) {
                Log.i(TAG, "Acking ${ackIds.size} relay envelope(s) on ${config.relayUrl}: $ackIds")
                // An ack that never landed leaves consumed rows in the
                // mailbox; skipping past them would strand them there until
                // expiry, so the frontier waits for the next pass to retry.
                try {
                    relayRequest { RelayClient.ackEnvelopes(config, ackIds, network) }
                } catch (e: Exception) {
                    rethrowFamilyRateLimit(e)
                    pageFullyProcessed = false
                    Log.w(TAG, "Failed to ack relay envelope(s) on ${config.relayUrl}: ${e.message}")
                }
            }
            if (!pageFullyProcessed) cursorsAdvancing = false
            if (cursorsAdvancing) {
                store.advanceRelayFetchCursor(cursorKey, page.nextCursor, true)
                // Only while sweeping. An ordinary pass writing its page
                // cursors here would leave behind sweep progress claiming
                // coverage of rows no sweep looked at -- and a non-zero
                // progress is also what tells the next pass a sweep is under
                // way.
                if (sweeping) store.advanceRelaySweepCursor(cursorKey, page.nextCursor, true)
            }
            // End the walk on an EMPTY page, never on a short one: a server
            // is free to clamp `limit=` below our ask, and reading a short
            // page as end-of-mailbox would strand every row above it -- which
            // in an ascending-id mailbox is all the new mail. Reaching here
            // with a non-empty page means the cursor stood still, which relayd
            // cannot produce -- so this is a bail-out, not end-of-mailbox, and
            // deliberately does NOT record a completed sweep.
            if (!relayFetchWalkContinues(page.envelopes.size.toUInt(), after, page.nextCursor)) {
                Log.w(TAG, "Relay ${config.relayUrl} returned rows without advancing the cursor; ending the walk")
                return true
            }
            after = page.nextCursor
            if (
                relayMailboxWalkAction(pagesFetched, envelopesFetched) ==
                RelayMailboxWalkAction.YIELD_AND_SCHEDULE_CONTINUATION
            ) {
                Log.i(
                    TAG,
                    "Relay ${config.relayUrl} mailbox walk yielding after " +
                        "$pagesFetched page(s)/$envelopesFetched envelope(s) at after=$after; " +
                        "continuation scheduled",
                )
                // Start the delay only after the entire multi-mailbox pass
                // finishes. Scheduling here could let the timer fire while a
                // later config is still running and collapse the continuation
                // into an immediate in-flight rerun.
                mailboxContinuationNeeded = true
                return true
            }
        }
        return answered
    }

    /**
     * Mailboxes this process has already walked in full.
     *
     * Deliberately in-memory, and deliberately *narrow*: [relaySweepDue]
     * schedules from the persisted timestamp, and consults this only for a
     * mailbox that has never recorded a completed sweep. There it stops a
     * store write that keeps failing from turning every pass into a full
     * walk. A cold start on a mailbox with a recent sweep no longer re-walks
     * anything.
     *
     * Its meaning is unchanged by the sweep resume cursor, and the two do not
     * overlap. This bounds the cost of a store that cannot be written to --
     * where nothing is persisted, so `sweepAfterId` reads 0 and cannot keep a
     * sweep due. Persisted progress bounds the cost of a mailbox too deep to
     * walk in one pass, which is a store working exactly as intended. One
     * failing write still costs one walk per process; it does not cost one
     * per pass, and it never did once this set was consulted.
     */
    private val sweptThisSession: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /**
     * Records that a walk reached the end of this mailbox: restarts the sweep
     * interval and clears the sweep's resume cursor. Only called on natural
     * termination -- a sweep cut short by the service stopping, the network
     * going away, a relay error, or simply running out of its per-pass budget
     * leaves the timestamp alone, so the next pass finishes the sweep from
     * where this one stopped rather than believing a partial re-walk.
     */
    private fun noteSweepCompleted(cursorKey: String, now: Long) {
        sweptThisSession.add(cursorKey)
        store.noteRelaySweepCompleted(cursorKey, now)
    }

    private fun distinctRelayConfigs(contacts: List<Contact>, fallbackConfig: RelayConfig?): List<RelayConfig> =
        buildList {
            fallbackConfig?.let { add(it) }
            for (contact in contacts) {
                val config = resolvedPollRelayConfig(contact, fallbackConfig) ?: continue
                if (!contains(config)) {
                    add(config)
                    Log.i(
                        TAG,
                        "Secondary relay poll config added for contact ${UserIdHex.encode(contact.userId)}: ${config.relayUrl}",
                    )
                }
            }
        }

    /**
     * Core owns the mailbox-routing policy (T11) so both shells resolve
     * identically.
     *
     * Null here means "no relay attempt for this contact right now", which
     * leaves the envelope queued for a later pass and for the BLE/LAN paths.
     * That is deliberately the answer for a *silent* endpoint, and it is not
     * the same answer a rejected one gets. A rejection proves the card is
     * wrong, so falling back to our own relay costs nothing and delivers
     * outright when both sides have since moved to the same new host. Silence
     * proves nothing -- the host may be rebooting -- and falling back would
     * post a cross-family contact's mail into our own mailbox, which they
     * never read. `relay_posted_at` is terminal, so that misroute would not be
     * a retry: the envelope would never be offered to the relay path again.
     */
    private fun resolvedRelayConfig(contact: Contact, fallbackConfig: RelayConfig?): RelayConfig? {
        if (!contactEndpointAnswering(contact)) return null
        return resolvedContactDeliveryRelay(
            contact.relayUrl,
            contact.relayToken,
            fallbackConfig?.relayUrl,
            fallbackConfig?.relayToken,
            contactEndpointUsable(contact),
        )?.let { RelayConfig(it.url, it.token) }
    }

    /**
     * Mirrors the written-off set into the observable the UI reads. Computed
     * from the streak alone, not from either current usability probe: a relay
     * stays *reported* stale while its periodic probe is due, so the
     * explanation in the chat does not blink out merely because one request is
     * temporarily permitted.
     */
    private fun publishStaleContactRelays() {
        val rejected = contactRelayRejections
            .filterValues { coreContactRelayIsStale(it.rejectStreak) }
            .keys
        val unreachable = contactRelayUnreachable
            .filterValues { coreContactRelayUnreachableIsStale(it.unreachableStreak) }
            .keys
        MeshConnectivityStatus.setStaleRelayContacts(
            rejected + unreachable,
        )
    }

    /**
     * Whether this contact's card endpoint has earned another attempt this
     * pass. False once the core's streak threshold is met, until the
     * re-probe window opens -- this is what turns the pre-fix ten-rejections-
     * a-minute loop into four probes a day.
     */
    private fun contactEndpointUsable(contact: Contact): Boolean {
        val rejection = contactRelayRejections[UserIdHex.encode(contact.userId)] ?: return true
        return coreContactRelayEndpointUsable(
            rejection.rejectStreak,
            rejection.rejectedAtMs,
            passNowMs,
        )
    }

    /**
     * Whether this contact's card endpoint has answered recently enough to be
     * worth spending a request on.
     *
     * The counterpart to [contactEndpointUsable] for the failure mode that has
     * no HTTP answer to classify: a host that was retired rather than a token
     * that was revoked. False rests the endpoint until the core's probe window
     * opens, which is what stops an address that will never respond from being
     * dialled on every pass forever -- and, since this pass's own provisional
     * observations count too, from being dialled once per queued envelope
     * inside a single pass. See [ContactRelaySilence.endpointAnswering].
     */
    private fun contactEndpointAnswering(contact: Contact): Boolean =
        contactRelaySilence.endpointAnswering(
            UserIdHex.encode(contact.userId),
            contactEndpointKey(contact),
            passNowMs,
        )

    /**
     * A rest belongs to an *address*, not to a person: `relayCursorKey` hashes
     * the contact's current endpoint so a card or a T23 notice that moves them
     * to a different host is tried again immediately instead of serving out
     * the old host's rest window.
     */
    private fun contactEndpointKey(contact: Contact): String =
        relayCursorKey(contact.relayUrl.orEmpty(), contact.relayToken.orEmpty())

    /**
     * CP4: fetch/ack/presence resolution. Post-CP4 friend cards carry
     * post-only deposit tokens, which cannot read a mailbox — the core
     * resolves a same-family card back to our own member config and drops
     * cross-family deposit endpoints entirely (polling them would just 403
     * `deposit_only` on every pass). Legacy member-token cards keep
     * proxy-polling exactly as before. Sends stay on [resolvedRelayConfig].
     */
    private fun resolvedPollRelayConfig(contact: Contact, fallbackConfig: RelayConfig?): RelayConfig? =
        resolvedContactDeliveryPollRelay(
            contact.relayUrl,
            contact.relayToken,
            fallbackConfig?.relayUrl,
            fallbackConfig?.relayToken,
            // Reading a mailbox that is not answering is the same waste as
            // posting to it; there is no fallback on this path either way.
            contactEndpointUsable(contact) && contactEndpointAnswering(contact),
        )?.let { RelayConfig(it.url, it.token) }

    /** Worst structured rejection of our own saved config during this pass (CP2b). */
    private var ownRelayFault: CoreRelayFault? = null

    /**
     * Rejection streaks against contacts' card endpoints, read once per pass
     * and consulted per contact (only contacts with a non-zero streak appear).
     * Kept up to date *within* a pass by [advanceContactRelayStreak] so the
     * envelopes after the one that tripped the threshold already route the new
     * way instead of finishing the pass against an endpoint we just wrote off.
     */
    private var contactRelayRejections: MutableMap<String, ContactRelayRejection> = mutableMapOf()

    /** Persisted transport-level failures, separate from HTTP rejections. */
    private var contactRelayUnreachable: MutableMap<String, ContactRelayUnreachable> = mutableMapOf()

    /**
     * Contacts whose streak already advanced during this pass.
     *
     * The core's threshold is worded in *passes* ("requiring the next pass to
     * agree" — see `CONTACT_RELAY_STALE_STREAK`), and that guarantee is what
     * rules out a relay answering mid-redeploy from a half-initialised
     * process. Counting per envelope quietly broke it: a contact with two
     * queued messages was written off inside a single pass, which is exactly
     * the false positive the second pass exists to prevent. One contact
     * contributes at most one step per pass, however many envelopes are
     * waiting for them.
     */
    private val contactRelayCountedThisPass: MutableSet<String> = mutableSetOf()

    /**
     * Persisted rests loaded into the in-pass breaker, plus the pass now
     * running's provisional observations. See [ContactRelaySilence].
     */
    private val contactRelaySilence = ContactRelaySilence()

    /** This pass's `now`, so streak timestamps and re-probe windows agree within a pass. */
    private var passNowMs: Long = 0L

    /** Epoch ms until which relayd asked us not to sync again; 0 = no backoff. */
    @Volatile private var rateLimitedUntilMs = 0L

    private var familyBackoffIdentityHash = 0
    private val familyRelayRequestPacer = FamilyRelayRequestPacer()
    private val familyRelayBackoff = FamilyRelayBackoff()

    /** Internal signal that unwinds every nested upload/fetch loop on a 429. */
    private class FamilyRateLimitAbort(cause: RelayHttpException) : RuntimeException(cause)

    private val rateLimitRetryRunnable = Runnable { requestRelaySync("rate limit retry") }
    private val mailboxContinuationRunnable = Runnable { requestRelaySync("mailbox continuation") }
    private var mailboxContinuationNeeded = false

    private fun <T> relayRequest(request: () -> T): T {
        val waitMs = familyRelayRequestPacer.reserve(SystemClock.elapsedRealtime())
        if (waitMs > 0L) Thread.sleep(waitMs)
        try {
            return request()
        } catch (error: RelayHttpException) {
            if (relayClassifyHttpError(error.code.toUShort(), error.relayCode) == CoreRelayFault.RATE_LIMITED) {
                val retryAfterMs = relayRetryAfterMs(error.retryAfter).toLong()
                val delayMs = familyRelayBackoff.onRateLimited(retryAfterMs, familyBackoffIdentityHash)
                rateLimitedUntilMs = maxOf(rateLimitedUntilMs, System.currentTimeMillis() + delayMs)
                ownRelayFault = worseRelayFault(ownRelayFault, CoreRelayFault.RATE_LIMITED)
                throw FamilyRateLimitAbort(error)
            }
            throw error
        }
    }

    private fun rethrowFamilyRateLimit(error: Exception) {
        if (error is FamilyRateLimitAbort) throw error
    }

    private fun scheduleMailboxContinuation() {
        handler.removeCallbacks(mailboxContinuationRunnable)
        handler.postDelayed(mailboxContinuationRunnable, relayMailboxContinuationDelayMs())
    }

    /**
     * Records a structured HTTP rejection when it concerns our OWN saved
     * config -- a contact's stale card relay failing is not our pass's
     * fault. Classification lives in the core (`relay_status.rs`); an
     * unstructured failure (OUTAGE) is not recorded because the pass's
     * success flags already express it as [RelayHealth.Failing].
     */
    private fun noteOwnRelayFault(config: RelayConfig, fallbackConfig: RelayConfig?, error: Exception) {
        if (config != fallbackConfig) return
        val http = error as? RelayHttpException ?: return
        val fault = relayClassifyHttpError(http.code.toUShort(), http.relayCode)
        if (fault == CoreRelayFault.OUTAGE) return
        ownRelayFault = worseRelayFault(ownRelayFault, fault)
    }

    /**
     * The other half of [noteOwnRelayFault], which had no owner before this:
     * a rejection from the endpoint in a CONTACT's friend card.
     *
     * [noteOwnRelayFault] deliberately ignores these ("not our pass's
     * fault"), and nothing else looked at them, so a card pointing at a
     * retired host produced an unbounded silent retry loop -- observed in the
     * field posting to a rebuilt relay ~10x/minute forever while the person's
     * messages sat at one tick. Recording the streak is what lets
     * [contactEndpointUsable] stop the loop and the UI say why.
     *
     * Only counts when [config] is genuinely the contact's own endpoint: once
     * we have fallen back to our own relay, a failure there is our own
     * relay's health, not evidence about their card.
     */
    private fun noteContactRelayFault(
        contact: Contact,
        config: RelayConfig,
        fallbackConfig: RelayConfig?,
        error: Exception,
    ) {
        if (config == fallbackConfig) return
        val http = error as? RelayHttpException
        if (http == null) {
            // No HTTP answer at all -- a retired host, dead DNS, a refused
            // connection, or a card URL this client will not dial. Not
            // evidence about the card on its own, so it is only remembered
            // here; commitUnreachableContactRelays decides at the end of the
            // pass whether this device had any business believing it.
            val key = UserIdHex.encode(contact.userId)
            // Log on the transition only. The per-envelope upload warnings name
            // the host but not whose card carries it, which left a field report
            // of hundreds of failures against one URL with no way to tell which
            // contact to ask for a fresh card. Once per contact per pass answers
            // that without restoring the volume the short-circuit just removed.
            if (contactRelaySilence.noteUnreachableThisPass(key, contactEndpointKey(contact))) {
                Log.w(TAG, "Contact $key relay ${config.relayUrl} did not answer: ${error.message}")
            }
            return
        }
        // Any HTTP response proves the endpoint is answering. A structured
        // rejection may advance its separate streak below, but must clear the
        // persisted transport-silence verdict first.
        noteContactRelayAnswered(contact)
        val fault = relayClassifyHttpError(http.code.toUShort(), http.relayCode)
        if (coreContactRelayStreakDelta(fault) == 0L) return
        val streak = advanceContactRelayStreak(contact) ?: return
        Log.w(
            TAG,
            "Contact ${UserIdHex.encode(contact.userId)} relay ${config.relayUrl} rejected us " +
                "($fault, streak=$streak); their friend card looks stale",
        )
    }

    /**
     * Advances one contact's persisted rejection streak, at most once per
     * pass, and reflects the new value in [contactRelayRejections] so the rest
     * of this pass already sees it. Returns the new streak, or null if this
     * contact was already counted.
     */
    private fun advanceContactRelayStreak(contact: Contact): Long? {
        val key = UserIdHex.encode(contact.userId)
        if (!contactRelayCountedThisPass.add(key)) return null
        val streak = store.noteContactRelayRejected(contact.userId, passNowMs)
        contactRelayRejections[key] = ContactRelayRejection(contact.userId, streak, passNowMs)
        return streak
    }

    /**
     * A successful post to a contact's own endpoint is the only thing that
     * clears its streak -- see `clear_contact_relay_rejection`'s doc for why
     * a transient fault deliberately does not. The endpoint answering also
     * settles the unreachable question outright, whatever this pass had
     * provisionally observed.
     */
    private fun noteContactRelaySuccess(contact: Contact, config: RelayConfig, fallbackConfig: RelayConfig?) {
        if (config == fallbackConfig) return
        val key = UserIdHex.encode(contact.userId)
        noteContactRelayAnswered(contact)
        if (!contactRelayRejections.containsKey(key)) return
        store.clearContactRelayRejection(contact.userId)
        contactRelayRejections.remove(key)
        contactRelayCountedThisPass.remove(key)
    }

    /** Any HTTP answer settles transport silence, including a non-2xx one. */
    private fun noteContactRelayAnswered(contact: Contact) {
        val key = UserIdHex.encode(contact.userId)
        contactRelaySilence.noteAnswered(key)
        store.clearContactRelayUnreachable(contact.userId)
        contactRelayUnreachable.remove(key)
    }

    /**
     * Turns this pass's silent endpoints into unreachable streaks.
     *
     * [ownRelayAnswered] is handed straight to the core rather than tested
     * here: whether same-pass proof of working internet is required, and what
     * the absence of it means, is one rule that both shells must answer
     * identically, so `core_contact_relay_unreachable_delta` is the only place
     * it is decided. Without the proof the delta is 0, nothing is recorded,
     * and the observation is discarded -- a phone in a tunnel fails every
     * endpoint at once, and resting them all would take the relay path away
     * from every contact for the whole rest window the moment connectivity
     * came back.
     */
    private fun commitUnreachableContactRelays(ownRelayAnswered: Boolean, contacts: List<Contact>) {
        val contactsByKey = contacts.associateBy { UserIdHex.encode(it.userId) }
        for ((key, _) in contactRelaySilence.commitPass(ownRelayAnswered, passNowMs)) {
            val contact = contactsByKey[key] ?: continue
            val endpointKey = contactEndpointKey(contact)
            val streak = store.noteContactRelayUnreachable(contact.userId, endpointKey, passNowMs)
            contactRelayUnreachable[key] = ContactRelayUnreachable(
                contact.userId,
                endpointKey,
                streak,
                passNowMs,
            )
            Log.w(
                TAG,
                "Contact $key relay endpoint did not answer while our own relay did " +
                    "(silent passes=$streak); resting it rather than retrying every pass",
            )
        }
    }

    private fun syncRelayPresence(
        config: RelayConfig,
        identity: Identity,
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
        now: Long,
        network: Network?,
    ) {
        // CP4: presence is a read, so contacts group under their *poll*
        // config — a family member's deposit-token card resolves back to our
        // own member config and their presence keeps flowing through it.
        val contactsForConfig = contacts.filter { contact ->
            resolvedPollRelayConfig(contact, fallbackConfig) == config
        }
        if (contactsForConfig.isEmpty()) return
        val announce = if (RelayConfigStore.shareOnline(context)) {
            recentPresenceHintsFor(identity.userId, now)
        } else {
            emptyList()
        }
        val query = dedupeHints(
            contactsForConfig.flatMap { contact -> recentPresenceHintsFor(contact.userId, now) },
        )
        if (announce.isEmpty() && query.isEmpty()) return
        val contactByHint = HashMap<String, Contact>(query.size)
        for (contact in contactsForConfig) {
            for (hint in recentPresenceHintsFor(contact.userId, now)) {
                contactByHint[UserIdHex.encode(hint)] = contact
            }
        }
        try {
            val localNow = System.currentTimeMillis()
            val page = relayRequest { RelayClient.syncPresence(config, announce, query, network) }
            for (presence in page.presence) {
                val contact = contactByHint[UserIdHex.encode(presence.hint)] ?: continue
                val ageMs = (page.nowMs - presence.lastSeenMs).coerceAtLeast(0L)
                val localSeenAt = localNow - ageMs
                MeshConnectivityStatus.mergePresenceLastSeen(UserIdHex.encode(contact.userId), localSeenAt)
                runCatching {
                    store.recordPeerConnectionEvent(
                        contact.userId,
                        PeerConnectionTransport.SHORE_PASS,
                        PeerConnectionEventKind.PRESENCE_SEEN,
                        localSeenAt,
                    )
                }
            }
            Log.i(
                TAG,
                "Synced relay presence on ${config.relayUrl}: announce=${announce.size} query=${query.size} hits=${page.presence.size}",
            )
        } catch (e: Exception) {
            rethrowFamilyRateLimit(e)
            Log.w(TAG, "Relay presence sync failed on ${config.relayUrl}: ${e.message}")
        }
    }
}
