package com.cruisemesh.app.mesh

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Handler
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.relay.RelayClient
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import com.cruisemesh.app.relay.RelayPushClient
import com.cruisemesh.app.relay.normalizeRelayUrl
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreRelayEnvelopeDisposition
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreGroupFanoutRows
import uniffi.cruisemesh_core.dedupeHints
import uniffi.cruisemesh_core.coreGroupFanoutRowsForCarried
import uniffi.cruisemesh_core.recentPresenceHintsFor
import uniffi.cruisemesh_core.relayFetchBatchLimit

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
                MeshConnectivityStatus.setRelayHealth(RelayHealth.NoInternet)
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
        MeshConnectivityStatus.setRelayHealth(
            when {
                configs.isEmpty() -> RelayHealth.NoConfig
                !hasValidatedInternet() -> RelayHealth.NoInternet
                else -> RelayHealth.Failing(System.currentTimeMillis())
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
     * (re)connect from [MessageStore.relayFetchHints] (mail addressed to us,
     * plus mail addressed to a contact we can proxy-fetch for, same as
     * [pollRelayMailbox]'s doc) so a
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
        relayPushClient.start(config) { onReady -> computeRelayPushHints(identity, onReady) }
    }

    /**
     * FA3: computes the relay-push hint set on the store executor and always
     * invokes [onReady] exactly once -- with the hints, with `emptyList()` if
     * the computation throws, or with `emptyList()` if the executor has
     * already been shut down (MeshService.onDestroy racing a pending
     * reconnect). [RelayPushClient.connect] depends on hearing back to decide
     * whether to open a socket or back off and retry (empty hints reads as
     * "nothing to subscribe to yet," same as before this fix); silently
     * dropping the callback would strand it never reconnecting.
     */
    private fun computeRelayPushHints(identity: Identity, onReady: (List<ByteArray>) -> Unit) {
        runOnStoreExecutorAlwaysReplying("relay push hint computation", { onReady(emptyList()) }) {
            assertOffMainThreadForStore("relay push hint computation")
            val now = System.currentTimeMillis()
            val computed = try {
                store.relayFetchHints(identity.userId, now)
            } catch (e: CoreException) {
                Log.w(TAG, "Failed to compute relay push hints: ${e.message}")
                emptyList()
            }
            onReady(computed)
        }
    }

    fun requestRelaySync(reason: String) {
        if (!isRunning() || identityProvider() == null) return
        if (!hasValidatedInternet()) {
            MeshConnectivityStatus.setRelayHealth(RelayHealth.NoInternet)
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
                } catch (e: Exception) {
                    Log.w(TAG, "Relay sync failed ($reason): ${e.message}")
                    MeshConnectivityStatus.setRelayHealth(RelayHealth.Failing(System.currentTimeMillis()))
                }
                val rerun = synchronized(relaySyncLock) {
                    if (relaySyncPending && isRunning() && hasValidatedInternet()) {
                        relaySyncPending = false
                        true
                    } else {
                        relaySyncInFlight = false
                        false
                    }
                }
                if (!rerun) break
            }
        }.start()
    }

    private fun performRelaySyncPass(reason: String) {
        val identity = identityProvider() ?: return
        val now = System.currentTimeMillis()
        store.pruneExpiredOutboundEnvelopes(now)
        store.pruneExpiredOutgoingReceiptEnvelopes(now)
        store.pruneExpiredCarried(now)
        val contacts = store.listContacts()
        val fallbackConfig = RelayConfigStore.load(context)
        // Bind this whole pass to a validated network when the default can't be
        // trusted (associated-but-dead Wi‑Fi, no VPN); otherwise null = use the
        // default (normal networks and VPN tunnels route themselves).
        val network = relayBindTarget()
        backfillOutgoingReceipts(identity, now)
        uploadPendingOutgoingReceiptEnvelopes(contacts, fallbackConfig, now, network)
        uploadPendingOutboundEnvelopes(contacts, fallbackConfig, now, network)
        uploadFamilyCarriedEnvelopes(contacts, fallbackConfig, now, network)

        val configs = distinctRelayConfigs(contacts, fallbackConfig)
        if (configs.isEmpty()) {
            MeshConnectivityStatus.setRelayHealth(RelayHealth.NoConfig)
            return
        }
        var anyRelaySucceeded = false
        var ownRelaySucceeded = fallbackConfig == null
        for (config in configs) {
            try {
                pollRelayMailbox(config, identity, now, network)
                syncRelayPresence(config, identity, contacts, fallbackConfig, now, network)
                anyRelaySucceeded = true
                if (config == fallbackConfig) ownRelaySucceeded = true
            } catch (e: Exception) {
                // A contact can carry stale relay credentials from an older
                // friend card. That relay failing must not abort polling of
                // the remaining relays or declare our own configured relay
                // unreachable when it succeeded.
                Log.w(TAG, "Relay sync failed for ${config.relayUrl}: ${e.message}")
            }
        }
        if (ownRelaySucceeded && anyRelaySucceeded) {
            MeshConnectivityStatus.setRelayHealth(RelayHealth.Ok(now))
        } else {
            MeshConnectivityStatus.setRelayHealth(RelayHealth.Failing(now))
        }
        val netDesc = if (network != null) "${networkLabel(network)}(pinned)" else "${networkLabel(connectivityManager.activeNetwork)}(default)"
        Log.i(TAG, "Relay sync complete: configs=${configs.size} net=$netDesc reason=$reason")
    }

    private fun uploadPendingOutgoingReceiptEnvelopes(
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
        now: Long,
        network: Network?,
    ) {
        val contactsByUserId = contacts.associateBy { UserIdHex.encode(it.userId) }
        for (envelope in store.pendingRelayOutgoingReceiptEnvelopes(RELAY_STORE_BATCH_LIMIT, now)) {
            val contact = contactsByUserId[UserIdHex.encode(envelope.recipientUserId)] ?: continue
            val config = resolvedRelayConfig(contact, fallbackConfig) ?: continue
            try {
                val relayId = RelayClient.postReceiptEnvelope(config, envelope, network)
                store.markOutgoingReceiptEnvelopeRelayPosted(envelope.msgId, now)
                Log.i(
                    TAG,
                    "Uploaded receipt envelope ${UserIdHex.encode(envelope.msgId)} to relay ${config.relayUrl} as id=$relayId",
                )
            } catch (e: Exception) {
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
        for (envelope in store.pendingRelayOutboundEnvelopes(RELAY_STORE_BATCH_LIMIT, now)) {
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
                        RelayClient.postOutboundEnvelope(config, envelope, network)
                        store.markOutboundEnvelopeRelayPosted(envelope.msgId, now)
                    } catch (e: Exception) {
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
                        RelayClient.postFanoutRow(config, row, network)
                        posted++
                    } catch (e: Exception) {
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
                val relayId = RelayClient.postOutboundEnvelope(config, envelope, network)
                store.markOutboundEnvelopeRelayPosted(envelope.msgId, now)
                Log.i(
                    TAG,
                    "Uploaded outbound envelope ${UserIdHex.encode(envelope.msgId)} to relay ${config.relayUrl} as id=$relayId",
                )
            } catch (e: Exception) {
                Log.w(TAG, "Failed to upload outbound envelope to relay ${config.relayUrl}: ${e.message}")
            }
        }
    }

    private fun relayConfigForGroupRecipient(
        groupId: ByteArray,
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
    ): RelayConfig? {
        val group = store.getGroup(groupId) ?: return fallbackConfig
        for (memberId in group.memberUserIds) {
            val contact = contacts.firstOrNull { it.userId.contentEquals(memberId) } ?: continue
            val config = resolvedRelayConfig(contact, fallbackConfig)
            if (config != null) return config
        }
        return fallbackConfig
    }

    private fun uploadFamilyCarriedEnvelopes(
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
        now: Long,
        network: Network?,
    ) {
        for (envelope in store.familyCarriedEnvelopes(RELAY_STORE_BATCH_LIMIT, now)) {
            val contact = store.contactMatchingHint(envelope.recipientHint, now)
            if (contact == null) {
                // Group-hinted carried envelope: previously skipped entirely
                // (no contact match). A member mule can now decompose it into
                // per-member fan-out rows (specs/group-relay-durability.md
                // §4.2) so the group's mail reaches internet-only members
                // through this phone's uplink too. No mark-posted concept for
                // carried rows -- re-posts every pass dedupe server-side via
                // the deterministic fan-out ids. Non-member mules still can't
                // recognize the hint and still skip, unchanged.
                val group = store.groupMatchingHint(envelope.recipientHint, now) ?: continue
                val config = relayConfigForGroupRecipient(group.id, contacts, fallbackConfig) ?: continue
                val rows = coreGroupFanoutRowsForCarried(
                    envelope.msgId,
                    group.memberUserIds,
                    envelope.hopTtl,
                    envelope.expiry,
                    envelope.sealed,
                )
                for (row in rows) {
                    try {
                        RelayClient.postFanoutRow(config, row, network)
                    } catch (e: Exception) {
                        Log.w(TAG, "Failed to upload carried fan-out row to relay ${config.relayUrl}: ${e.message}")
                    }
                }
                continue
            }
            val config = resolvedRelayConfig(contact, fallbackConfig) ?: continue
            try {
                val relayId = RelayClient.postCarriedEnvelope(config, envelope, network)
                Log.i(
                    TAG,
                    "Uploaded carried envelope ${UserIdHex.encode(envelope.msgId)} to relay ${config.relayUrl} as id=$relayId",
                )
            } catch (e: Exception) {
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
     * over BLE/LAN is now also acked (DTN_TODOS.md §3.1) instead of being
     * re-fetched on every pass until expiry -- see
     * [CoreRelayEnvelopeDisposition]'s KDoc for the exact rule.
     *
     * The un-acked proxy envelopes DO still advance the cursor within this
     * pass (`after = page.nextCursor` is unconditional), so the inner loop
     * still terminates the same way it always did; they simply get
     * re-fetched on the next poll pass. That's bounded and cheap (deduped by
     * [GossipState.seenIds] on the way in) but not free -- see the TODO below
     * for the follow-up that would avoid the re-fetch entirely.
     *
     * TODO(relay-proxy-polling follow-ups):
     *  - A persistent per-contact proxy cursor (like `after`, but remembered
     *    across passes instead of restarting at 0) would let us skip
     *    re-fetching already-seen-but-still-CARRIED envelopes on every pass,
     *    at the cost of a bit more state to persist and reconcile.
     *  - [MessageStore.relayProxyHints] fetches every contact's hints on
     *    every pass, so its cost scales with contact-list size. Fine for this
     *    app's small family circles; would need a smarter server-side "for
     *    this family token" fan-out if that ever became a large flat social
     *    graph.
     */
    private fun pollRelayMailbox(
        config: RelayConfig,
        identity: Identity,
        now: Long,
        network: Network?,
    ) {
        val fetchHints = store.relayFetchHints(identity.userId, now)
        if (fetchHints.isEmpty()) return
        var after = 0L
        val fetchBatchLimit = relayFetchBatchLimit().toInt()
        while (isRunning() && hasValidatedInternet()) {
            val page = RelayClient.fetchEnvelopes(config, fetchHints, after, fetchBatchLimit, network)
            Log.i(
                TAG,
                "Fetched ${page.envelopes.size} relay envelope(s) from ${config.relayUrl} after=$after next=${page.nextCursor}",
            )
            if (page.envelopes.isEmpty()) return
            val dispositions = ArrayList<CoreRelayEnvelopeDisposition>(page.envelopes.size)
            for (envelope in page.envelopes) {
                val disposition = processRelayEnvelope(envelope, identity)
                dispositions += CoreRelayEnvelopeDisposition(
                    relayId = envelope.id,
                    msgId = envelope.msgId,
                    disposition = disposition,
                    recipientHint = envelope.recipientHint,
                )
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
                RelayClient.ackEnvelopes(config, ackIds, network)
            }
            after = page.nextCursor
            if (page.envelopes.size < fetchBatchLimit) return
        }
    }

    private fun distinctRelayConfigs(contacts: List<Contact>, fallbackConfig: RelayConfig?): List<RelayConfig> =
        buildList {
            fallbackConfig?.let { add(it) }
            for (contact in contacts) {
                val config = resolvedRelayConfig(contact, fallbackConfig) ?: continue
                if (!contains(config)) add(config)
            }
        }

    private fun resolvedRelayConfig(contact: Contact, fallbackConfig: RelayConfig?): RelayConfig? {
        val relayUrl = normalizeRelayUrl(contact.relayUrl.orEmpty())
        val relayToken = contact.relayToken?.trim().orEmpty()
        if (relayUrl.isNotEmpty() && relayToken.isNotEmpty()) {
            return RelayConfig(relayUrl, relayToken)
        }
        return fallbackConfig
    }

    private fun syncRelayPresence(
        config: RelayConfig,
        identity: Identity,
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
        now: Long,
        network: Network?,
    ) {
        val contactsForConfig = contacts.filter { contact ->
            resolvedRelayConfig(contact, fallbackConfig) == config
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
            val page = RelayClient.syncPresence(config, announce, query, network)
            for (presence in page.presence) {
                val contact = contactByHint[UserIdHex.encode(presence.hint)] ?: continue
                val ageMs = (page.nowMs - presence.lastSeenMs).coerceAtLeast(0L)
                val localSeenAt = localNow - ageMs
                MeshConnectivityStatus.mergePresenceLastSeen(UserIdHex.encode(contact.userId), localSeenAt)
            }
            Log.i(
                TAG,
                "Synced relay presence on ${config.relayUrl}: announce=${announce.size} query=${query.size} hits=${page.presence.size}",
            )
        } catch (e: Exception) {
            Log.w(TAG, "Relay presence sync failed on ${config.relayUrl}: ${e.message}")
        }
    }
}
