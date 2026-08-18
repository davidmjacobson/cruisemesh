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
import com.cruisemesh.app.relay.CoreRelayDriver
import com.cruisemesh.app.relay.RelayClient
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayEngineSettings
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import com.cruisemesh.app.relay.RelayHttpException
import com.cruisemesh.app.relay.RelayPassEngine
import com.cruisemesh.app.relay.RelayPushClient
import com.cruisemesh.app.relay.RelayPushSubscription
import com.cruisemesh.app.relay.RelayRotationDriver
import com.cruisemesh.app.devicelink.RosterGossipSender
import com.cruisemesh.app.relay.RelayUpdateSender
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreRelayContactConfig
import uniffi.cruisemesh_core.CoreRelayEndpointConfig
import uniffi.cruisemesh_core.CoreRelayFault
import uniffi.cruisemesh_core.CoreRelayPassOutcome
import uniffi.cruisemesh_core.CoreRelayPassPlan
import uniffi.cruisemesh_core.CoreRelayShadowLane
import uniffi.cruisemesh_core.CoreRelayTransportError
import uniffi.cruisemesh_core.CoreRelayNetworkVerdict
import uniffi.cruisemesh_core.CoreRelayRoaming
import uniffi.cruisemesh_core.coreRelayNetworkPermitted
import uniffi.cruisemesh_core.coreRelayPassDefaultBudgets
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
import uniffi.cruisemesh_core.relayMailboxContinuationDelayMs
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

    /** Network facts only: core owns the roaming/cost decision. */
    private fun relayNetworkVerdict(): CoreRelayNetworkVerdict {
        val network = relayBindTarget() ?: connectivityManager.activeNetwork
        val caps = network?.let { connectivityManager.getNetworkCapabilities(it) }
            ?: return CoreRelayNetworkVerdict.PERMITTED
        return coreRelayNetworkPermitted(
            // Only a cellular path can roam. Reading the capability alone
            // would let a Wi-Fi network that happens not to carry
            // NOT_ROAMING read as roaming and silently switch Shore Pass off.
            roaming = if (!caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) ||
                caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_ROAMING)
            ) {
                CoreRelayRoaming.NO
            } else {
                CoreRelayRoaming.YES
            },
            constrained = connectivityManager.restrictBackgroundStatus ==
                ConnectivityManager.RESTRICT_BACKGROUND_STATUS_ENABLED,
            userAllowsRoaming = RelayEngineSettings.allowsRoamingData(context),
        )
    }

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
     * as [RelayMailboxWalker.walk]'s [MessageStore.relayFetchHints] doc, plus one
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
        if (identity == null || config == null || !hasValidatedInternet() ||
            relayNetworkVerdict() == CoreRelayNetworkVerdict.DEFERRED_ROAMING) {
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
        if (relayNetworkVerdict() == CoreRelayNetworkVerdict.DEFERRED_ROAMING) {
            // A policy deferral is offline-like: no request, retry, failure
            // streak, endpoint rest, or rate-limit state is touched.
            MeshConnectivityStatus.setRelayHealth(RelayHealth.DeferredRoaming)
            updateRelayPushSubscription()
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
                        canSync = isRunning() && hasValidatedInternet() &&
                            relayNetworkVerdict() != CoreRelayNetworkVerdict.DEFERRED_ROAMING,
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
        // The engine is chosen once, here, and not consulted again. A pass
        // that could change engines partway through would leave a cursor with
        // no single owner and a transcript nobody can read; rollback is
        // "the next pass runs the other engine", nothing finer.
        if (RelayEngineSettings.passEngine(context) == RelayPassEngine.CORE) {
            performCoreRelaySyncPass(identity, reason)
            return
        }
        val now = System.currentTimeMillis()
        val shadow = shadowAdapter.beginPass(now)
        familyBackoffIdentity = identity.userId
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
        // §10.2's driver, before the T23 read below rather than after it.
        // A rotation that lands adopts a new endpoint, which is an endpoint
        // change like any other -- and the announce stage that notices one is
        // the same stage that clears the carried-upload and group fan-out
        // markers naming the mailbox we just left. Running this first is what
        // lets a rotation ride out on the pass that performed it.
        rotateFamilyTokenIfOwed(identity)
        // T23: if our own endpoint changed since the last announcement, queue
        // the notice to every contact *before* this pass uploads, so it rides
        // out in the same sync. This is the single trigger for every way the
        // config can change (Shore Pass setup and removal, manual entry in
        // Advanced, a scanned setup card, a backup restore) because they all
        // already end in `RelaySyncEvents.requestSync()` — no save site has to
        // remember to announce, and none can be missed. `announceIfChanged` is
        // idempotent, so the periodic poll re-entering here costs nothing.
        RelayUpdateSender.announceIfChanged(context, store, identity)
        // DL-3 / §9.5 / §10.1's contact leg, on the same catch-up footing and
        // for the same reason: this person's own device list is a fact every
        // contact has to learn, and core's per-contact ledger makes asking on
        // every pass cost one query on the install that has nothing to say --
        // which is nearly every install. The link and remove journeys fire it
        // at the moment they change something; this is the repair pass that
        // catches a contact added since, or a copy that expired unread.
        RosterGossipSender.announceIfOwed(store, identity)
        // Taken *before* the uploads, because that is the moment core plans
        // from: `CoreRelayPassPlan.contacts` is built once at the top of a
        // pass, while both breakers this reads are advanced by the loops below
        // as rows fail. Reading them afterwards would score legacy's
        // per-row decisions against an end-of-pass plan core would never have
        // made, and the first contact whose streak tipped mid-pass would
        // report a destination divergence that never happened.
        val shadowContacts = shadowContacts(contacts)
        try {
            uploadPendingOutgoingReceiptEnvelopes(contacts, fallbackConfig, now, network, shadow)
            uploadPendingOutboundEnvelopes(contacts, fallbackConfig, now, network, shadow)
            if (relayNetworkVerdict() != CoreRelayNetworkVerdict.DEFERRED_CONSTRAINED) {
                uploadFamilyCarriedEnvelopes(contacts, fallbackConfig, now, network, shadow)
            }
        } finally {
            // Compared here rather than at the end of the pass: the two lanes
            // this slice speaks for are done, and everything after them
            // belongs to a later package. In a `finally` because a family rate
            // limit unwinds straight out of those loops, and a sample spent on
            // a pass that was cut short is still evidence about the rows it
            // did reach. Nothing the comparison finds can change any of it.
            shadowAdapter.finishPass(shadow, fallbackConfig, shadowContacts, now)
        }

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
        val pages = relayMailboxPages(network)
        for (config in configs) {
            try {
                val walk = mailboxWalker.walk(config, identity, now, pages)
                if (walk.continuationNeeded) mailboxContinuationNeeded = true
                syncRelayPresence(config, identity, contacts, fallbackConfig, now, network)
                anyRelaySucceeded = true
                if (config == fallbackConfig) {
                    ownRelaySucceeded = true
                    if (walk.answered) ownRelayAnswered = true
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

    // -----------------------------------------------------------------------
    // The core engine
    // -----------------------------------------------------------------------

    /**
     * One relay pass, run by `CoreRelayPass` instead of by the code above.
     *
     * Notice what is not here. No prune, no announce decision, no batch
     * selection, no destination resolution, no request, no status branch, no
     * cursor, no ack, no marker, no silence evidence, no continuation
     * arithmetic: core made every one of those and this method never sees
     * them. What it does is assemble the facts core cannot read from the store
     * — this device's own pass, its contacts' cards, the hints it fetches
     * under, whether its endpoint changed, and the quiet window already in
     * force — hand them over, execute the actions that come back, and project
     * the summary onto the things Android owns: a health pill, a retry timer,
     * a continuation.
     *
     * Off by default. [RelayEngineSettings.passEngine] selects it, and until
     * canary evidence says otherwise the answer is the legacy engine. The
     * remaining known gap is named here rather than left for someone to find
     * by flipping it: a group-addressed row is posted as one row to one
     * mailbox instead of being fanned out per member, so a group's mail would
     * not arrive. It is recorded as an open divergence in
     * `specs/protocol-contract-v1.md` §5.2.
     *
     * What used to sit beside it, and no longer does: an ingested page now
     * reaches [processRelayEnvelope] through [projector], so it raises the
     * same notification the legacy walk raises; a presence answer now reaches
     * [MeshConnectivityStatus] the same way; and a contact endpoint resting
     * for *silence* is now told apart from one that was rejected, because the
     * plan below carries both brakes rather than folding them into one.
     *
     * What is deliberately *not* on that list is anything the shell still owns
     * on both paths. The prunes, the receipt backfill, the silence breaker's
     * pass boundaries and the announce are all run here exactly as the legacy
     * pass runs them, because they are inputs core reads rather than decisions
     * core makes; leaving one out would not be a divergence to measure, it
     * would be a lane that quietly stopped.
     */
    private fun performCoreRelaySyncPass(identity: Identity, reason: String) {
        val now = System.currentTimeMillis()
        familyBackoffIdentity = identity.userId
        passNowMs = now
        val contacts = store.listContacts()
        val fallbackConfig = RelayConfigStore.load(context)
        contactRelayRejections = store.listContactRelayRejections()
            .associateByTo(mutableMapOf()) { UserIdHex.encode(it.userId) }
        contactRelayUnreachable = store.listContactRelayUnreachable()
            .associateByTo(mutableMapOf()) { UserIdHex.encode(it.userId) }
        contactRelayCountedThisPass.clear()
        // The silence breaker is state this shell keeps, and core's
        // `endpoint_usable` reads it. Without the restore it holds nothing, so
        // every rested endpoint answers "still answering" and a retired
        // friend-card host is re-dialled on every pass; without the pass
        // boundary an observation from an earlier pass can suppress an
        // endpoint that is now healthy.
        contactRelaySilence.restore(contactRelayUnreachable.values)
        contactRelaySilence.beginPass()
        anyRelayConfigKnown = fallbackConfig != null ||
            contacts.any { resolvedPollRelayConfig(it, fallbackConfig) != null }
        if (fallbackConfig == null && !anyRelayConfigKnown!!) {
            MeshConnectivityStatus.setRelayHealth(RelayHealth.NoConfig)
            return
        }
        publishStaleContactRelays()
        // The only producer of relay-uploadable outgoing receipt envelopes,
        // and it belongs to neither engine: it refreshes the durable receipt
        // envelope for the current delivered/read watermarks and records the
        // ids in the seen set so our own receipts coming back off the relay
        // dedupe instead of being re-carried as foreign mail. Core's prune
        // stage only prunes. Skipping it here would silently stop delivered
        // and read ticks from propagating over the relay while the pass
        // reported a healthy, empty receipt lane.
        backfillOutgoingReceipts(identity, now)
        // §10.2's driver, before the T23 read below rather than after it.
        // A rotation that lands adopts a new endpoint, which is an endpoint
        // change like any other -- and stage 2 is what clears the
        // carried-upload and group fan-out markers naming the mailbox we just
        // left. Running this first is what lets a rotation ride out on the
        // pass that performed it.
        rotateFamilyTokenIfOwed(identity)
        // Read before the announce, not after: `announceIfChanged` is what
        // *records* the epoch as announced, so asking afterwards always
        // answers "nothing changed" and core's announce stage would be
        // unreachable from this shell.
        val ownEndpointChanged = RelayConfigStore.relayEpoch(context) >
            RelayConfigStore.announcedRelayEpoch(context)
        RelayUpdateSender.announceIfChanged(context, store, identity)
        // DL-3 / §9.5 / §10.1's contact leg, on the same catch-up footing and
        // for the same reason: this person's own device list is a fact every
        // contact has to learn, and core's per-contact ledger makes asking on
        // every pass cost one query on the install that has nothing to say --
        // which is nearly every install. The link and remove journeys fire it
        // at the moment they change something; this is the repair pass that
        // catches a contact added since, or a copy that expired unread.
        RosterGossipSender.announceIfOwed(store, identity)

        val network = relayBindTarget()
        val plan = CoreRelayPassPlan(
            own = fallbackConfig?.let { CoreRelayEndpointConfig(it.relayUrl, it.relayToken) },
            // Both brakes, distinctly: see [coreRelayContactConfigs] for what
            // each one means and why folding them together misroutes mail.
            contacts = coreRelayContactConfigs(
                contacts,
                endpointUsable = ::contactEndpointUsable,
                endpointAnswering = ::contactEndpointAnswering,
            ),
            ownUserId = identity.userId,
            fetchHints = store.relayFetchHints(identity.userId, now),
            presenceAnnounce = if (RelayConfigStore.shareOnline(context)) {
                recentPresenceHintsFor(identity.userId, now)
            } else {
                emptyList()
            },
            presenceQuery = dedupeHints(contacts.flatMap { recentPresenceHintsFor(it.userId, now) }),
            ownEndpointChanged = ownEndpointChanged,
            sweptThisSession = coreEngineSweptThisSession,
            consecutiveRateLimits = familyRelayBackoff.consecutiveRateLimits.toUInt(),
            quietUntilMs = rateLimitedUntilMs,
            budgets = coreRelayPassDefaultBudgets(),
        )

        val runner = CoreRelayPassRunner(
            store = store,
            executor = { passId, actionId, request, atMs ->
                // Paced by the same family request pacer the legacy engine
                // uses: the budget belongs to the family's relay token, not to
                // whichever engine happens to be spending it.
                val waitMs = familyRelayRequestPacer.reserve(SystemClock.elapsedRealtime())
                if (waitMs > 0L) Thread.sleep(waitMs)
                CoreRelayDriver.execute(passId, actionId, request, network, atMs) {
                    !isRunning() || !hasValidatedInternet()
                }
            },
            clock = { System.currentTimeMillis() },
            isCancelled = { !isRunning() || !hasValidatedInternet() },
            onProjection = { projection ->
                projector.project(projection, identity, contacts, System.currentTimeMillis())
            },
        )
        val summary = runner.run(plan, "cp")
        // Only a pass that ran every stage to the end has actually swept every
        // mailbox, and `relay_sweep_due` answers "no" for a device that has
        // never swept once this says yes. A pass cancelled before its first
        // fetch, refused inside a quiet window, or cut short by a 429 has swept
        // nothing; marking it swept anyway would leave a fresh install unable
        // to reach anything below its frontier for the life of the process.
        if (summary.outcome == CoreRelayPassOutcome.COMPLETED) {
            coreEngineSweptThisSession = true
        }

        MeshConnectivityStatus.setRelayHealth(relayHealthFor(summary.health, now))
        // The quiet window core recorded at the refusal, adopted as a floor
        // here for the same reason it is one there: a later, shorter window
        // must never be able to lower it.
        if (summary.quietUntilMs > 0L) {
            rateLimitedUntilMs = maxOf(rateLimitedUntilMs, summary.quietUntilMs)
        }
        // `RATE-01`'s escalation lives in a counter this shell holds and core
        // reads back through the plan, so a core pass has to keep it moving on
        // both sides. Only these two outcomes may touch it: a completed pass is
        // the one thing that proves the punishment has been served, and any
        // other ending -- a refusal inside the quiet window, a cancellation, a
        // budget yield -- is a pass that spent nothing and learned nothing.
        // Clearing on those would let every pending nudge inside a rate-limit
        // window re-zero the streak, so a relay that kept refusing would be
        // answered with the base delay forever instead of a widening one.
        when (summary.outcome) {
            CoreRelayPassOutcome.RATE_LIMITED ->
                // For the count alone; core already decided the window this
                // refusal earns and put it in the summary.
                familyRelayBackoff.onRateLimited(0L, familyBackoffIdentity)
            CoreRelayPassOutcome.COMPLETED -> {
                familyRelayBackoff.onSuccessfulPass()
                if (summary.quietUntilMs <= System.currentTimeMillis()) rateLimitedUntilMs = 0L
            }
            else -> Unit
        }
        contactRelayRejections = store.listContactRelayRejections()
            .associateByTo(mutableMapOf()) { UserIdHex.encode(it.userId) }
        contactRelayUnreachable = store.listContactRelayUnreachable()
            .associateByTo(mutableMapOf()) { UserIdHex.encode(it.userId) }
        publishStaleContactRelays()
        Log.i(
            TAG,
            "Core relay pass complete: outcome=${summary.outcome} stage=${summary.stageReached} " +
                "requests=${summary.requestsIssued} ingested=${summary.rowsIngested} reason=$reason",
        )
        // `PROGRESS-01` already decided whether more work was earned and when,
        // and `RATE-01` already decided when this device may speak to the
        // family relay again. All this shell owns is which of its two existing
        // timers honours the answer -- the coalescing rate-limit retry when a
        // quiet window is open, so every nudge arriving inside it lands on one
        // wake rather than each arming its own, and the continuation timer
        // otherwise.
        val quietRemainingMs = rateLimitedUntilMs - System.currentTimeMillis()
        if (quietRemainingMs > 0L) {
            handler.removeCallbacks(rateLimitRetryRunnable)
            handler.postDelayed(rateLimitRetryRunnable, quietRemainingMs)
            return
        }
        summary.continuation?.let { continuation ->
            val delay = (continuation.notBeforeMs - System.currentTimeMillis()).coerceAtLeast(0L)
            handler.removeCallbacks(mailboxContinuationRunnable)
            handler.postDelayed(mailboxContinuationRunnable, delay)
        }
    }

    /**
     * Where a core pass's committed work reaches this device's surfaces.
     *
     * The same inbound call the legacy walk makes, so a notification, a chat
     * row and a receipt look identical whichever engine fetched the envelope;
     * and the same "last seen" merge the legacy presence sync makes. See
     * [CoreRelayPassProjector] for why the disposition it returns is ignored.
     */
    private val projector = CoreRelayPassProjector(
        deliver = { envelope, identity -> processRelayEnvelope(envelope, identity) },
        mergePresence = MeshConnectivityStatus::mergePresenceLastSeen,
        notePresenceSeen = { userId, seenAtMs ->
            store.recordPeerConnectionEvent(
                userId,
                PeerConnectionTransport.SHORE_PASS,
                PeerConnectionEventKind.PRESENCE_SEEN,
                seenAtMs,
            )
        },
    )

    /**
     * Whether the core engine has already swept every mailbox in this process,
     * which is `relay_sweep_due`'s input. In memory on purpose: correctness
     * must not depend on recovering an in-memory session, and a restart simply
     * sweeps once more.
     */
    private var coreEngineSweptThisSession = false

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
        shadow: RelayShadowPassCapture?,
    ) {
        val contactsByUserId = contacts.associateBy { UserIdHex.encode(it.userId) }
        val skipRecipients = unpostableRecipients(contacts, fallbackConfig)
        shadow?.noteSkippedRecipients(skipRecipients)
        for (envelope in store.pendingRelayOutgoingReceiptEnvelopes(RELAY_STORE_BATCH_LIMIT, now, skipRecipients)) {
            val contact = contactsByUserId[UserIdHex.encode(envelope.recipientUserId)]
            val config = contact?.let { resolvedRelayConfig(it, fallbackConfig) }
            if (contact == null || config == null) {
                // Declined, with the reason captured as "no mailbox resolved"
                // rather than as nothing at all: a row one engine posts and
                // the other silently drops is exactly what the canary is for.
                shadow?.noteDeclined(CoreRelayShadowLane.RECEIPT, envelope.msgId, envelope.hopTtl, envelope.recipientHint, envelope.recipientUserId, envelope.sealed.size, envelope.expiry)
                continue
            }
            try {
                val relayId = relayRequest { RelayClient.postReceiptEnvelope(config, envelope, network) }
                store.markOutgoingReceiptEnvelopeRelayPosted(envelope.msgId, now)
                noteContactRelaySuccess(contact, config, fallbackConfig)
                shadow?.noteSucceeded(CoreRelayShadowLane.RECEIPT, envelope.msgId, envelope.hopTtl, envelope.recipientHint, envelope.recipientUserId, envelope.sealed.size, envelope.expiry, config)
                Log.i(
                    TAG,
                    "Uploaded receipt envelope ${UserIdHex.encode(envelope.msgId)} to relay ${config.relayUrl} as id=$relayId",
                )
            } catch (e: Exception) {
                shadow?.noteFailed(CoreRelayShadowLane.RECEIPT, envelope.msgId, envelope.hopTtl, envelope.recipientHint, envelope.recipientUserId, envelope.sealed.size, envelope.expiry, config, e)
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
        shadow: RelayShadowPassCapture?,
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
                //
                // Explicitly outside what this slice's canary can speak for.
                // The core upload lanes post a row to one resolved mailbox and
                // do not decompose a group-addressed row into member rows, so
                // there is no core plan to compare a fan-out against; counting
                // it keeps a clean report from reading as a claim about groups.
                // Counted in *posted rows* rather than in queue entries: one
                // group envelope becomes one row per member on the wire, and a
                // report that could not speak for twelve rows must not say one.
                val group = store.getGroup(envelope.recipientUserId)
                shadow?.noteUnshadowed(group?.memberUserIds?.size?.coerceAtLeast(1) ?: 1)
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
            val config = resolvedRelayConfig(contact, fallbackConfig)
            if (config == null) {
                shadow?.noteDeclined(CoreRelayShadowLane.AUTHORED, envelope.msgId, envelope.hopTtl, envelope.recipientHint, envelope.recipientUserId, envelope.sealed.size, envelope.expiry)
                continue
            }
            try {
                val relayId = relayRequest { RelayClient.postOutboundEnvelope(config, envelope, network) }
                store.markOutboundEnvelopeRelayPosted(envelope.msgId, now)
                noteContactRelaySuccess(contact, config, fallbackConfig)
                shadow?.noteSucceeded(CoreRelayShadowLane.AUTHORED, envelope.msgId, envelope.hopTtl, envelope.recipientHint, envelope.recipientUserId, envelope.sealed.size, envelope.expiry, config)
                Log.i(
                    TAG,
                    "Uploaded outbound envelope ${UserIdHex.encode(envelope.msgId)} to relay ${config.relayUrl} as id=$relayId",
                )
            } catch (e: Exception) {
                shadow?.noteFailed(CoreRelayShadowLane.AUTHORED, envelope.msgId, envelope.hopTtl, envelope.recipientHint, envelope.recipientUserId, envelope.sealed.size, envelope.expiry, config, e)
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
        shadow: RelayShadowPassCapture?,
    ) {
        // Carried mail starves like the outbound and receipt queues: a failed
        // upload leaves the row unmarked, so under flat order one unreachable
        // destination refills the batch every pass. Core resolves each row's
        // rotating recipient hint to a contact so it can partition and skip.
        val skipRecipients = unpostableRecipients(contacts, fallbackConfig)
        for (envelope in store.familyCarriedEnvelopes(RELAY_STORE_BATCH_LIMIT, now, skipRecipients)) {
            val contact = store.contactMatchingHint(envelope.recipientHint, now)
            if (contact == null) {
                // A group-hinted carried row fans out per member, and this
                // lane has no shadow at all: counted so a mule's report never
                // reads as agreement about rows nobody compared.
                shadow?.noteUnshadowed(
                    store.groupMatchingHint(envelope.recipientHint, now)
                        ?.memberUserIds?.size?.coerceAtLeast(1) ?: 1,
                )
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
            // A carried 1:1 row: one row on the wire, and one this slice's
            // canary does not model. Carried uploads belong to a later
            // package.
            shadow?.noteUnshadowed(1)
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
     * The relay mailbox walk itself, which used to be a method here.
     *
     * It moved out because it was the one part of this class with no test: it
     * needed a [Context], a [ConnectivityManager] and a live relay to run at
     * all, so the composition of the core's walk rules -- where the #270 sweep
     * livelock actually lived -- could only be checked by reading it. The walk
     * keeps the state that must outlive a pass (which mailboxes this process
     * has swept in full); everything Android-shaped stays on this side of
     * [RelayMailboxPages].
     */
    private val mailboxWalker = RelayMailboxWalker(
        store = store,
        processRelayEnvelope = processRelayEnvelope,
        canWalk = { isRunning() && hasValidatedInternet() },
    )

    /**
     * The walk's relay requests for one sync pass: pinned to that pass's bound
     * [network], paced and rate-limit-aware through [relayRequest] exactly as
     * every other relay call in this class is.
     */
    private fun relayMailboxPages(network: Network?): RelayMailboxPages = object : RelayMailboxPages {
        override fun fetch(
            config: RelayConfig,
            hints: List<ByteArray>,
            after: Long,
            limit: Int,
            onShrink: (Int, Int) -> Unit,
        ) = relayRequest {
            RelayClient.fetchEnvelopesWithinResponseCap(config, hints, after, limit, network, onShrink)
        }

        override fun ack(config: RelayConfig, relayIds: List<Long>) {
            relayRequest { RelayClient.ackEnvelopes(config, relayIds, network) }
        }

        override fun abortsPass(error: Exception): Boolean = error is FamilyRateLimitAbort

        override fun reopenPushSocket(config: RelayConfig) {
            relayPushClient.resubscribe(config)
        }
    }

    /**
     * **§10 step 2, driven from the pass that owns the network.**
     *
     * A device removal writes the rotation down and returns immediately; this
     * is where it actually happens. Here rather than in the removal journey for
     * the reasons this class exists at all: the bind target that keeps relay
     * traffic off a dead Wi-Fi and out of a VPN, and the discipline about how
     * often a family's relay may be spoken to, both live on this side of the
     * seam. [RelayRotationDriver] paces itself, so a pass that finds a rotation
     * it may not retry yet costs one query.
     *
     * Failure never touches the pass. A rotation is a repair the fleet owes
     * itself, not a precondition for moving mail, and a relay that refuses to
     * re-key must not also stop messages being delivered.
     */
    private fun rotateFamilyTokenIfOwed(identity: Identity) {
        try {
            val driver = RelayRotationDriver.forApp(context, store, relayBindTarget())
            // Both directions of §10.2's own-device leg, in the order that
            // matters: a device that was told about a rotation writes it down
            // before it could waste an attempt asking about one of its own.
            driver.adoptAnnouncedCredential()
            driver.rotateIfPending(identity)
        } catch (e: Exception) {
            Log.w(TAG, "Relay token rotation did not finish this pass: ${e.message}")
        }
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

    /**
     * This device's public user id, captured at the start of a pass so a 429
     * anywhere inside it can draw the family's stable anti-lockstep offset.
     * The offset itself is derived in the core; this shell no longer hashes
     * anything (see [FamilyRelayBackoff]).
     */
    private var familyBackoffIdentity: ByteArray = ByteArray(0)
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
                val delayMs = familyRelayBackoff.onRateLimited(retryAfterMs, familyBackoffIdentity)
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

    /** Whether this failure ends the whole pass rather than one lane's row. */
    private fun abortsPass(error: Exception): Boolean = error is FamilyRateLimitAbort

    // -----------------------------------------------------------------------
    // The migration canary
    // -----------------------------------------------------------------------

    /**
     * Compares a few legacy passes a day against what the core engine would
     * have planned for the receipt and authored lanes. It performs no network
     * work, writes nothing but a diagnostics record, and refuses to run when
     * the core engine is the one moving mail. See [RelayShadowAdapter].
     */
    private val shadowAdapter = RelayShadowAdapter(
        // The store's one diagnostics write, and not the store: what the
        // canary cannot reach, it cannot become a second writer through.
        sink = store::noteRelayShadowReport,
        passEngine = { RelayEngineSettings.passEngine(context) },
        shadowEnabled = { RelayEngineSettings.shadowEnabled(context) },
        loadSampler = { RelayEngineSettings.shadowSampler(context) },
        saveSampler = { RelayEngineSettings.setShadowSampler(context, it) },
    )

    /**
     * The contacts, as a routing decision reads them: card fields plus whether
     * this device is still willing to spend a request on that card's endpoint.
     *
     * The two shell-side breakers collapse into core's one usability flag
     * here, and the collapse is worth naming: a contact rested for *silence*
     * gets no relay attempt at all on this shell, while core's flag means
     * "ignore the card and fall back to our own mailbox". Mapping both onto
     * one flag is what makes the canary report the difference rather than hide
     * it — see the divergence table in `specs/protocol-contract-v1.md` §5.2.
     */
    private fun shadowContacts(contacts: List<Contact>): List<CoreRelayShadowContact> =
        contacts.map { contact ->
            CoreRelayShadowContact(
                userId = contact.userId,
                relayUrl = contact.relayUrl,
                relayToken = contact.relayToken,
                endpointUsable = contactEndpointUsable(contact) && contactEndpointAnswering(contact),
            )
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
