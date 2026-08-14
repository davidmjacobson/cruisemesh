package com.cruisemesh.app.mesh

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.bluetooth.BluetoothA2dp
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.atomic.AtomicLong
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.MainActivity
import com.cruisemesh.app.R
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.identity.TermsAcceptanceStore
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreSprayGate
import uniffi.cruisemesh_core.CoreSprayTrigger
import uniffi.cruisemesh_core.DigestEntry
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.LanEndpointProvenance
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.PeerConnectionEventKind
import uniffi.cruisemesh_core.PeerConnectionTransport
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.digestIsSharedGroup
import uniffi.cruisemesh_core.encodeDigest
import uniffi.cruisemesh_core.coreIsHiddenSprayKind
import uniffi.cruisemesh_core.coreOwnCapabilities
import uniffi.cruisemesh_core.encodeHello
import uniffi.cruisemesh_core.encodeHello2
import uniffi.cruisemesh_core.encodeLanEndpoint
import uniffi.cruisemesh_core.encodeTransportProbe
import uniffi.cruisemesh_core.parseFrame

private const val TAG = "MeshService"
private const val NOTIFICATION_CHANNEL_ID = "cruisemesh_mesh_status"
private const val NOTIFICATION_ID = 1
private const val OPEN_APP_REQUEST_CODE = 1001
private const val STOP_SERVICE_REQUEST_CODE = 1002
private const val ENABLE_BLUETOOTH_REQUEST_CODE = 1003
private const val LAN_HEALTH_INTERVAL_MS = 30_000L
// D8: how often to check whether a long-lived link is due for a re-digest. The
// actual 3-5 min jittered gate lives in core (`spray_policy.rs`, which asks
// `should_redigest`); this only sets the polling granularity.
private const val DIGEST_MAINTENANCE_INTERVAL_MS = 60_000L

internal fun bluetoothAudioConnectedFromProfileState(state: Int): Boolean? = when (state) {
    BluetoothProfile.STATE_CONNECTED -> true
    BluetoothProfile.STATE_DISCONNECTED -> false
    else -> null
}

// Battery, 2026-07-21: the relay poll cadence itself now comes from
// RadioPowerPolicy.relayPollIntervalMs (900s while RelayPushClient's WS push
// is healthy, 60s otherwise, 5s right after a healthy->down transition) --
// see scheduleRelayPolling/relayPollRunnable/onRelayPushHealthChanged. This
// constant is now only how often the duty-mode policy re-checks itself for a
// quiet period elapsing with no new link event (see radioPowerRunnable).
private const val RADIO_POWER_CHECK_INTERVAL_MS = 30_000L

/**
 * Bound on how many of our own carried `msg_id`s [seedSeenIdsFromOwnHistory]
 * re-seeds into [GossipState.seenIds] at startup.
 *
 * DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): this used to also be the cap
 * on the outgoing DIGEST's advertised `msg_id` list, but that decision now
 * lives in core (`engine.rs::DIGEST_ADVERTISED_MSG_IDS_LIMIT`, behind
 * [MessageStore.coreDigestAdvertisedMsgIds]) so both platforms share one
 * source of truth. This constant now only bounds the unrelated seeding
 * query below; it's kept at the same value as a reasonable, previously
 * proven bound, not because the two uses need to match.
 */
private const val SEEN_ID_SEED_CARRIED_LIMIT: ULong = 512uL

/**
 * Runs both BLE GATT roles simultaneously (DESIGN.md §5.2) so this device can
 * be discovered by, and discover, any other CruiseMesh phone in range.
 *
 * Milestone 1 wiring: frames are real signed/sealed envelopes (DESIGN.md
 * §6.3, §7.1) exchanged over [MeshRouter], not the Milestone-0 plaintext
 * greeting. [MeshRouter] is registered with this service's two live
 * transports on start and torn down on stop, so [com.cruisemesh.app.chat.MeshSender]
 * can reach a connected contact without this service being anything but a
 * transport implementation detail to it.
 *
 * FA15: this class now owns only the transports, link/session lifecycle
 * (HELLO/DIGEST bookkeeping), radio-power policy, and Android plumbing. The
 * envelope pipeline -- including the wire-`chatId` convention doc that used
 * to live here -- moved to [InboundEnvelopeProcessor], and the relay
 * networking to [RelaySyncEngine]; both are constructed in [onStartCommand]
 * with this service as the composition root.
 */
class MeshService : Service() {

    // @Volatile: read/written from the main thread (lifecycle, restarts) but
    // also read from the receive-path threads below (central-GATT binder,
    // peripheral-GATT binder, LanTransport's connectionExecutor, and the
    // relay-sync thread) via processInboundEnvelope/status checks -- see the
    // threading-model note on processInboundEnvelope. Plain fields here would
    // let a receive-path thread observe a stale cached value.
    @Volatile
    private var identity: Identity? = null
    private lateinit var store: MessageStore

    /**
     * FA15: the extracted envelope pipeline and relay engine. Created in
     * [onStartCommand] once the store and identity exist; null until then.
     * Deliberately NOT nulled in [onDestroy]: a late frame on a receive-path
     * thread after teardown gets processed against the (harmless) dying
     * instance, exactly as it did when this was all one class.
     */
    private var envelopeProcessor: InboundEnvelopeProcessor? = null
    private var relaySync: RelaySyncEngine? = null

    /**
     * Group digests answered on each live link. The 1:1 fallback still
     * resends any shared group this link has not answered for. Record only
     * after the spray gate allows — a gated first digest must not suppress
     * later catch-up.
     */
    private val groupDigestAnswers = GroupDigestAnswers()

    /** Cached once; avoids re-reading [android.content.pm.ApplicationInfo.flags] on every [assertOffMainThreadForStore] call. */
    private val isDebuggableBuild: Boolean by lazy { DebugFileLog.isDebuggableBuild(this) }

    /**
     * FA3 accept criterion: debug-build assert that the three paths this fix
     * moved onto [storeExecutor] (seeding, D8 digest maintenance, relay-push
     * hint computation) never touch [MessageStore] from the main thread
     * again. No-op in release builds -- [isDebuggableBuild] is a cached
     * boolean, so the common-case cost there is one comparison against
     * `false` before returning.
     *
     * Deliberately scoped to just those call sites rather than wrapping
     * every [store] access in MeshService: [handleChatViewed] (registered
     * with [ChatViewEvents], invoked synchronously from [MainActivity]'s UI
     * thread for the read-receipt-on-view flow) calls `store` on the main
     * thread today, via the same [sendReceiptOnAddress]/[store] plumbing the
     * receive path also uses off-thread -- a pre-existing, out-of-scope call
     * path FA3 does not touch. A blanket guard on [store] itself would trip
     * on that legitimate path on every debug build, so this stays an
     * explicit call at the top of each of the four functions below instead
     * (the fallback this item's own acceptance note allows: "if a full guard
     * is impractical, guard the three fixed paths and note it").
     */
    private fun assertOffMainThreadForStore(where: String) {
        if (!isDebuggableBuild) return
        check(Looper.myLooper() != Looper.getMainLooper()) {
            "FA3: $where must not touch MessageStore on the main thread -- route it through storeExecutor"
        }
    }

    /**
     * FA3: single background thread MeshService uses for [MessageStore] work
     * that used to run on the main thread -- seeding [GossipState.seenIds] at
     * startup ([seedSeenIdsFromOwnHistory]), the initial relay-health publish
     * ([publishInitialRelayHealth]), D8 digest maintenance
     * ([checkDigestMaintenance]), and relay-push hint computation
     * ([updateRelayPushSubscription]). One thread, not a pool: these call
     * sites already ran serially on whichever thread invoked them before this
     * fix (main-thread lifecycle/handler callbacks), MessageStore's SQLite
     * backing gains nothing from parallel access here, and a single thread
     * keeps this easy to reason about alongside the four *other* concurrent
     * receive-path threads ([InboundEnvelopeAdmission]'s KDoc) that already
     * call into [store] independently of this one.
     *
     * Results that reach outward from here -- [MeshRouter.sendToAddress] (via
     * [checkDigestMaintenance]) and the [MutableStateFlow][kotlinx.coroutines.flow.MutableStateFlow]-backed
     * [MeshConnectivityStatus]/[GossipState] writes -- are safe to call from
     * any thread already (see each type's own thread-safety notes), so
     * nothing here needs to post back to the main thread; only
     * [computeRelayPushHints]'s result crosses back into [RelayPushClient],
     * which is itself safe to drive from a background thread (its own state
     * is `@Synchronized`).
     *
     * Stopped in [onDestroy] via [ExecutorService.shutdown] (graceful --
     * finishes whatever task already started, e.g. an in-flight digest send,
     * rather than [ExecutorService.shutdownNow]'s best-effort interrupt) once
     * every producer that could submit new work is already stopped.
     */
    private val storeExecutor: ExecutorService = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "MeshService-store").apply { isDaemon = true }
    }

    /**
     * Submits [block] to [storeExecutor], catching both a thrown exception
     * (logged rather than crashing the process the way an uncaught exception
     * on this background thread otherwise would -- matching how the
     * main-thread call sites this replaces already guarded their own
     * [uniffi.cruisemesh_core.CoreException] cases) and
     * [RejectedExecutionException] (the executor is already shut down, e.g. a
     * late timer fire racing [onDestroy]). Fire-and-forget: use
     * [computeRelayPushHints] instead for the one caller
     * ([updateRelayPushSubscription]) that needs a result back.
     */
    private fun runOnStoreExecutor(label: String, block: () -> Unit) {
        try {
            storeExecutor.execute {
                try {
                    block()
                } catch (e: Exception) {
                    Log.e(TAG, "FA3: $label failed on storeExecutor", e)
                }
            }
        } catch (e: RejectedExecutionException) {
            Log.w(TAG, "FA3: $label dropped; storeExecutor already shut down", e)
        }
    }

    /**
     * Same as [runOnStoreExecutor], but for a caller that has already
     * promised someone else a reply ([computeRelayPushHints] promising
     * [RelayPushClient] a hint list back): [onFailure] runs -- once, on
     * whichever thread hit the problem -- if the task can't even be
     * submitted ([RejectedExecutionException]) or if [block] itself throws
     * something [block] didn't already catch. Never silently drops the
     * caller's expected reply the way plain [runOnStoreExecutor] would.
     */
    private fun runOnStoreExecutorAlwaysReplying(label: String, onFailure: () -> Unit, block: () -> Unit) {
        try {
            storeExecutor.execute {
                try {
                    block()
                } catch (e: Exception) {
                    Log.e(TAG, "FA3: $label failed on storeExecutor", e)
                    onFailure()
                }
            }
        } catch (e: RejectedExecutionException) {
            Log.w(TAG, "FA3: $label dropped; storeExecutor already shut down", e)
            onFailure()
        }
    }

    @Volatile
    private var running = false
    @Volatile
    private var meshRolesRunning = false
    private var bluetoothAudioConnected = false
    private var bluetoothAudioReceiverRegistered = false
    private var bluetoothStateReceiverRegistered = false
    private var lanTransport: LanTransport? = null
    private val lanHealthTracker = LanHealthTracker()
    private val lanProbeNonce = AtomicLong(System.nanoTime())

    /**
     * Holds a LAN endpoint hint that arrived on a link before that address's
     * HELLO (DESIGN.md §5.2/§7.2) registered its userId with [MeshRouter] --
     * ordinary frame reordering, or the BLE congestion burst
     * [BlePeripheral]'s notify-failure tolerance now survives instead of
     * misreading as a dead link (Pixel 10 Pro field log, 2026-07-17). See
     * [handleLanEndpointHint] (stashes) and [handleHello] (replays).
     */
    private val pendingLanHints = PendingLanHintHold()

    /**
     * Coalesces the failover resume fan-out per logical peer -- see
     * [scheduleFailoverResume] and the core's `CoreFailoverResumeDebounce`.
     */
    private val failoverResumeDebounce = FailoverResumeDebounce()

    /**
     * The one pending [scheduleDeferredSpray] timer per logical peer (keyed by
     * hex user id), kept so a newer deferral can cancel the older one instead of
     * stacking a second burst behind it. Guarded by its own monitor: HELLO and
     * DIGEST frames for one peer can arrive on different binder threads.
     */
    private val pendingSprayDeferrals = mutableMapOf<String, Runnable>()

    /**
     * A peer DIGEST that arrived inside a peripheral spray-cooldown window,
     * held (keyed by hex user id) until [resumeLogicalPeerSync] can answer it.
     *
     * Dropping it instead is what the first cut did, and it is wrong in a way
     * that is easy to miss: [handleDigest] is the only link path that resends
     * the receipts we owe a peer, the authored messages its watermark says it is
     * missing, and the carried-copy confirmations that let the store retire what
     * the peer already holds. None of that comes back on its own -- the peer's
     * own digest-maintenance tick is minutes away, and it is our *response* that
     * is missing, not our digest. Holding the contents costs one small entry per
     * peer, replaced by any newer digest and taken when the deferral fires.
     * Guarded by its own monitor (frames arrive on binder threads).
     */
    private val gatedDigests = mutableMapOf<String, GatedDigest>()
    private val relayMainHandler by lazy { Handler(Looper.getMainLooper()) }
    private val bluetoothManager by lazy {
        getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
    }
    private val connectivityManager by lazy {
        getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
    }
    // T15 phase 2/3: keeps an internet-less Wi‑Fi association alive so the LAN
    // transport keeps reaching nearby phones on ship/captive Wi‑Fi, and reports
    // when that association drops so we can nudge the user (see refreshWifiHold).
    private val wifiHold by lazy { WifiAssociationHold(connectivityManager, ::onWifiAssociationLost) }
    @Volatile private var meshJoinedAtMs: Long = 0L

    /**
     * Contacts that have demonstrated LAN support, as UserID hex to the
     * millisecond that support was last seen. Cached because
     * [countUnlinkedCapableContacts] answers the LAN transport's
     * automatic-scan gate on the main handler every few seconds, and
     * recomputing it there means a SQLite contact list plus a
     * SharedPreferences read per contact on the UI thread. Refreshed off the
     * main thread by [refreshLanCapableContacts]; recency is applied at read
     * time so an entry going stale needs no refresh at all.
     */
    @Volatile private var lanCapableContacts: Map<String, Long> = emptyMap()
    private val lanEndpointCache by lazy { LanEndpointCache(this) }
    private val a2dpAudioBackoff = A2dpAudioBackoff()

    private val peripheral by lazy {
        BlePeripheral(this, ::onFrameReceived, ::onPeripheralCentralSubscribed, ::onPeripheralCentralDisconnected)
    }
    private val central by lazy {
        BleCentral(this, ::onFrameReceived, ::onCentralPeerConnected, ::onCentralPeerDisconnected)
    }

    /**
     * Battery, 2026-07-21: shared BLE scan/advertise duty-mode + relay-poll
     * cadence decisions -- see [RadioPowerPolicy]'s class doc for the
     * escalate/dwell rules. [evaluateRadioPower] gathers the inputs below and
     * pushes the result to [central]/[peripheral] on every real change; both
     * of their setters are idempotent, so this can (and does) get called
     * unconditionally from every link-connect/disconnect callback plus
     * [radioPowerRunnable]'s periodic tick (the only way to notice a quiet
     * period elapsing with no new event).
     */
    private val radioPowerPolicy = RadioPowerPolicy()

    /** Seeded from [PowerManager.isInteractive] when the screen receiver registers; kept current by [screenStateReceiver]. */
    @Volatile private var screenInteractive: Boolean = true

    /** Wall-clock time of the most recent link connect/disconnect across every transport (BLE central/peripheral, LAN); 0 = none yet this process. */
    @Volatile private var lastLinkChangeAtMs: Long = 0L

    /**
     * T22: wall-clock time the carry queue last *grew*, or 0 if it has not
     * grown this process. Refreshed off [storeExecutor] by
     * [refreshCarryQueueSignal] on every [radioPowerRunnable] tick.
     *
     * This used to be a plain `carriedLen() > 0` boolean, which measured
     * "are we holding any carried envelope at all". In a family that
     * actually uses the app that is permanently true -- carried 1:1
     * envelopes survive until digest-proof of receipt and envelopes live up
     * to 7 days -- so the escalation latched on and the radio never returned
     * to LOW_POWER. Confirmed on hardware 2026-07-27: every sampled
     * `evaluateRadioPower` line reported BALANCED, not one LOW_POWER.
     *
     * Freshly arrived mail is the part worth spending radio on, so this
     * tracks arrivals and [RadioPowerPolicy] treats them like link churn:
     * escalate now, relax once the window passes. Mail that is merely
     * sitting in the queue (already uploaded and waiting on a receipt, or
     * addressed to someone already linked) no longer holds the radio up.
     */
    @Volatile private var carryQueueLastGrewAtMs: Long = 0L

    /** Last carried-envelope count seen by [refreshCarryQueueSignal]; -1 before the first read. */
    @Volatile private var lastCarriedLen: Long = -1L

    private var screenStateReceiverRegistered = false
    private val screenStateReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            screenInteractive = when (intent?.action) {
                Intent.ACTION_SCREEN_ON -> true
                Intent.ACTION_SCREEN_OFF -> false
                else -> return
            }
            evaluateRadioPower("screen ${if (screenInteractive) "on" else "off"}")
        }
    }
    private val bluetoothAudioReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            val action = intent?.action ?: return
            val state = intent.getIntExtra(BluetoothProfile.EXTRA_STATE, -1)
            refreshBluetoothAudioStatus(
                "$action state=$state",
                bluetoothAudioConnectedFromProfileState(state),
            )
        }
    }
    /**
     * Restarts (or tears down) the BLE roles when the Bluetooth adapter is
     * toggled off and back on. Without this, turning Bluetooth off invalidates
     * the OS-side scanner/advertiser/GATT server, and because [startMeshRoles]
     * is guarded on [meshRolesRunning] the app never rebuilds them when
     * Bluetooth returns -- the device silently stops participating in the mesh
     * until the whole app is restarted (observed live 2026-07-10: a phone whose
     * Bluetooth was toggled received nothing over BLE even though the service
     * still reported "Mesh running").
     */
    private val bluetoothStateReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent?.action != BluetoothAdapter.ACTION_STATE_CHANGED) return
            when (intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, BluetoothAdapter.ERROR)) {
                BluetoothAdapter.STATE_ON -> {
                    Log.i(TAG, "Bluetooth turned on; restarting mesh roles")
                    if (running) restartMeshRoles()
                }
                BluetoothAdapter.STATE_TURNING_OFF, BluetoothAdapter.STATE_OFF -> {
                    Log.i(TAG, "Bluetooth turning off; stopping mesh roles")
                    stopMeshRoles()
                }
            }
            refreshRuntimeState()
            refreshForegroundNotification()
        }
    }
    private val lanHealthRunnable = object : Runnable {
        override fun run() {
            checkLanHealth()
            relayMainHandler.postDelayed(this, LAN_HEALTH_INTERVAL_MS)
        }
    }

    /**
     * Battery, 2026-07-21: periodic catch-all for [RadioPowerPolicy]'s duty
     * mode -- every other trigger ([screenStateReceiver], the six
     * link-connect/disconnect callbacks) is event-driven and calls
     * [evaluateRadioPower] directly, but a quiet period simply *elapsing*
     * with no new event needs something to notice it, hence this tick. Also
     * where [carryQueueLastGrewAtMs] gets refreshed, since that requires
     * a [storeExecutor] hop (see [refreshCarryQueueSignal]).
     */
    private val radioPowerRunnable = object : Runnable {
        override fun run() {
            refreshCarryQueueSignal()
            if (running) relayMainHandler.postDelayed(this, RADIO_POWER_CHECK_INTERVAL_MS)
        }
    }
    // The map that used to live here -- `lastDigestAtByAddress` -- is gone.
    // It was written by every spray and read by only one of them (the
    // maintenance tick), so the two event-driven call sites sprayed
    // unconditionally; see issue #280 and `core/src/spray_policy.rs`. Cadence,
    // budgets, identical-set suppression and receipt-quiet backoff are now one
    // core decision, consulted through [SprayPolicy].

    /**
     * FA3: the actual [checkDigestMaintenance] pass (store.chatDigest +
     * store.coreDigestAdvertisedMsgIds() per live link) now runs on
     * [storeExecutor], not this [relayMainHandler]-driven Runnable directly.
     * The re-arm (`postDelayed`) happens *inside* the executor task, after
     * the pass completes, exactly mirroring the original ordering where
     * `checkDigestMaintenance()` ran to completion before the next
     * `postDelayed` -- so a slow pass still self-throttles instead of a fresh
     * check queuing up behind an unfinished one every 60s regardless of how
     * long the last one took. The re-arm is in a `finally` rather than a
     * plain follow-on statement: [runOnStoreExecutor] catches and logs a
     * thrown exception rather than crashing the process the way the old
     * main-thread version would have -- without the `finally`, one bad pass
     * would silently and permanently stop the recurring check instead of
     * just failing that one pass, trading a loud crash for a quiet feature
     * death, which is strictly worse. The `if (running)` guard on the re-arm
     * closes the one race this executor hop introduces: [onDestroy] calling
     * [cancelDigestMaintenance] can no longer reliably remove "the next
     * pending callback" the way it could when this ran synchronously on the
     * main thread, because by the time this task's `finally` runs,
     * [onDestroy] may already have moved on -- so the re-arm itself checks
     * [running] (already `false` by the time [onDestroy] reaches
     * [cancelDigestMaintenance]) instead of relying on that call alone.
     */
    private val digestMaintenanceRunnable = object : Runnable {
        override fun run() {
            runOnStoreExecutor("digest maintenance") {
                try {
                    checkDigestMaintenance()
                } finally {
                    if (running) relayMainHandler.postDelayed(this, DIGEST_MAINTENANCE_INTERVAL_MS)
                }
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            Log.i(TAG, "Stopping mesh at the user's request")
            MeshStartupPreferences.setMeshEnabled(this, false)
            MeshRuntimeStatus.markStopped()
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }

        // Any start initiated by the app is durable user intent, matching the
        // iOS "Mesh running" switch. BootReceiver only reaches this path when
        // that same preference is already enabled.
        MeshStartupPreferences.setMeshEnabled(this, true)
        startForeground(NOTIFICATION_ID, buildNotification())
        if (!TermsAcceptanceStore.isCurrentVersionAccepted(this)) {
            MeshRuntimeStatus.markStopped()
            Log.i(TAG, "Current Terms of Use not accepted; stopping mesh service")
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
        // Debug builds: ensure log capture is running even if the process was
        // revived straight into the service without the UI (no-op in release
        // and idempotent with MainActivity's call).
        DebugFileLog.start(this)
        // Same reason: a process revived straight into the service does relay
        // work with no UI, and without this its log would carry relay traffic
        // and never say which pass produced it -- the gap this line closes.
        RelayConfigStore.logSummary(this)
        MeshRuntimeStatus.markStarting()

        if (running) {
            // Second "Start mesh" tap (or any repeat start) while already
            // running: everything below is live -- re-running it would at
            // best duplicate registrations and at worst disturb the BLE
            // roles (see BlePeripheral.start's idempotence note; this guard
            // makes that one redundant in the normal path but both stay, as
            // defense-in-depth).
            MeshRuntimeStatus.markActive()
            runOnStoreExecutor("initial relay health (repeat start)") { relaySync?.publishInitialRelayHealth() }
            Log.i(TAG, "onStartCommand: mesh already running; ignoring")
            return START_STICKY
        }

        if (!hasRequiredPermissions()) {
            MeshRuntimeStatus.markStopped()
            Log.w(TAG, "Missing BLE permissions; stopping")
            stopSelf()
            return START_NOT_STICKY
        }

        val loadedIdentity = IdentityStore.load(this)
        if (loadedIdentity == null) {
            // Shouldn't happen in practice -- MainActivity generates and
            // persists an identity on first launch, well before the mesh
            // can be started (DESIGN.md §6.2) -- but sealing/opening
            // requires one, so there's nothing useful this service can do
            // without it.
            MeshRuntimeStatus.markStopped()
            Log.e(TAG, "No identity persisted; stopping mesh service")
            stopSelf()
            return START_NOT_STICKY
        }
        identity = loadedIdentity
        MeshRouter.setLocalUserId(loadedIdentity.userId)
        store = AppStore.get(this)
        // Spray decisions carry no store of their own, so this is where they
        // find the protocol-event ring. Before the mesh roles come up, so the
        // first reconnect of the session is already recorded.
        SprayPolicy.attachEventJournal(store)
        // FA3: was a synchronous main-thread call here (full outbound-envelope
        // scans for every contact/group plus carried ids) -- now dispatched to
        // storeExecutor without blocking the mesh-role startup below on it.
        // Ordering: this races GossipState.seenIds against the mesh roles
        // coming up and receiving real traffic, but seedSeenIdsFromOwnHistory's
        // own KDoc already establishes that an un-seeded msg_id is a harmless,
        // not unsafe, gap -- worst case a mule handing back one of our own
        // envelopes before it's seeded gets misclassified as foreign and
        // re-carried/re-uploaded once (the relay and this store both dedupe by
        // msg_id too, per that KDoc). D4's dedupe/admission invariant
        // (InboundEnvelopeAdmission, processInboundEnvelope's KDoc) doesn't
        // depend on the seed either: it protects "don't double-deliver a copy
        // we're actively processing right now," which holds regardless of
        // whether GossipState.seenIds already contains an old entry for it.
        // So this is the "process anyway, dedupe idempotently" side of that
        // tradeoff, not "block startup on the seed."
        runOnStoreExecutor("seed seenIds from own history") { seedSeenIdsFromOwnHistory(loadedIdentity) }

        // FA15 composition root: the envelope pipeline and relay engine are
        // plain classes; everything they need from this service's state
        // (identity, running, the LAN transport, the FA3 executor) crosses as
        // small injected functions so the visibility/threading semantics stay
        // exactly what they were when this was one class.
        val processor = InboundEnvelopeProcessor(
            context = this,
            store = store,
            identityProvider = { identity },
            requestRelaySync = { reason -> relaySync?.requestRelaySync(reason) },
            lan = object : InboundEnvelopeProcessor.LanHooks {
                override fun sendLanEndpointHintTo(address: String) =
                    this@MeshService.sendLanEndpointHintTo(address)

                override fun connectToLanHint(hint: Frame.LanEndpoint, peerUserId: ByteArray) {
                    lanTransport?.connectToHint(hint, peerUserId)
                }

                override fun saveHintedLanEndpoint(
                    networkId: String?,
                    userId: ByteArray,
                    endpoint: LanManualEndpoint,
                ) = lanEndpointCache.save(networkId, userId, endpoint, LanEndpointProvenance.HINTED)

                override fun currentLanNetworkId(): String? = lanTransport?.currentNetworkId()

                override fun onLanCapabilityChanged() = refreshLanCapableContacts()
            },
        )
        envelopeProcessor = processor
        relaySync = RelaySyncEngine(
            context = this,
            store = store,
            handler = relayMainHandler,
            connectivityManager = connectivityManager,
            identityProvider = { identity },
            isRunning = { running },
            processRelayEnvelope = processor::handleRelayEnvelope,
            backfillOutgoingReceipts = processor::backfillRelayOutgoingReceiptEnvelopes,
            onRelayNetworkChanged = ::refreshWifiHold,
            assertOffMainThreadForStore = ::assertOffMainThreadForStore,
            runOnStoreExecutorAlwaysReplying = ::runOnStoreExecutorAlwaysReplying,
        )

        val lan = LanTransport(
            context = applicationContext,
            identity = loadedIdentity,
            trustedPeerForStaticKey = { remoteStaticKey ->
                trustedLanPeerUserId(store.listContacts(), remoteStaticKey)
            },
            unlinkedCapableContacts = ::countUnlinkedCapableContacts,
            onNetworkReady = ::onLanNetworkReady,
            onEndpointObserved = { userId, endpoint, networkId ->
                // An address the contact hinted at, already checked against
                // this phone's own network by the transport. Still only a
                // claim until a handshake completes, so it is filed unproven.
                lanEndpointCache.save(networkId, userId, endpoint, LanEndpointProvenance.HINTED)
            },
            onAuthenticated = ::onLanPeerAuthenticated,
            onDisconnected = ::onLanPeerDisconnected,
            onFrameReceived = ::onFrameReceived,
        )
        lanTransport = lan
        MeshRouter.registerCentral(central::sendFrame)
        MeshRouter.registerPeripheral(peripheral::sendFrame)
        MeshRouter.registerLan(lan::sendFrame)
        ChatViewEvents.register(processor::handleChatViewed)
        RelaySyncEvents.register { relaySync?.requestRelaySync("queue changed") }

        running = true
        runOnStoreExecutor("initial relay health") { relaySync?.publishInitialRelayHealth() }
        registerBluetoothAudioReceiver()
        registerBluetoothStateReceiver()
        relaySync?.registerRelayNetworkCallback()
        registerScreenStateReceiver()
        meshJoinedAtMs = System.currentTimeMillis()
        WifiTipStore.refresh(this)
        refreshWifiHold()
        relaySync?.scheduleRelayPolling()
        LanTransportDiagnostics.registerProbeRequester(::requestManualLanProbe)
        scheduleLanHealth()
        scheduleDigestMaintenance()
        scheduleRadioPowerChecks()
        // Seed the initial BLE duty mode before the roles below actually
        // start scanning/advertising, so [startMeshRoles] picks up the right
        // mode on its first start() call instead of defaulting to LOW_POWER
        // and immediately restarting -- lastLinkChangeAtMs is still 0 here
        // (no link has ever changed this process) so this is driven purely
        // by [screenInteractive] and the carry queue's last-known state.
        evaluateRadioPower("service start")
        lan.start()
        // The mesh runs regardless of Bluetooth audio now (see
        // refreshBluetoothAudioStatus); start the roles unconditionally rather
        // than gating them on an audio-clear check. (startMeshRoles is a no-op
        // at the BLE layer if Bluetooth is off; the state receiver restarts the
        // roles for real once Bluetooth is turned on.)
        startMeshRoles()
        // Mesh is up. Publish the real state (ACTIVE, or NO_BLUETOOTH if
        // Bluetooth is currently off) rather than an unconditional "running", so
        // the status pill can't claim the mesh is live while it's actually deaf.
        refreshRuntimeState()
        refreshBluetoothAudioStatus("service start")
        relaySync?.requestRelaySync("service start")
        relaySync?.updateRelayPushSubscription()
        return START_STICKY
    }

    override fun onDestroy() {
        running = false
        // T21: the ongoing mesh notification must never outlive the service.
        // stopForeground(STOP_FOREGROUND_REMOVE) used to run only on the
        // explicit ACTION_STOP path, so every other teardown (stopSelf, a
        // system stop, an eviction that still runs onDestroy) left a
        // notification claiming the mesh was up. Measured on a family phone:
        // two notifications posted, zero services running, for ~2 hours. Same
        // trust-breaking class as the B5 zombie transport header. A SIGKILL
        // never reaches onDestroy, which is why MainActivity also reconciles
        // stale notifications on launch.
        stopForeground(STOP_FOREGROUND_REMOVE)
        lanTransport?.stop()
        lanTransport = null
        MeshRuntimeStatus.markStopped()
        unregisterBluetoothAudioReceiver()
        unregisterBluetoothStateReceiver()
        relaySync?.unregisterRelayNetworkCallback()
        unregisterScreenStateReceiver()
        wifiHold.stop()
        relaySync?.cancelRelayPolling()
        relaySync?.stopPush()
        cancelLanHealth()
        cancelDigestMaintenance()
        cancelRadioPowerChecks()
        // A debounced failover resume can still be pending on relayMainHandler.
        // Its posted callback re-checks `running` (already false above) before
        // submitting anything to storeExecutor, so it cannot outlive this;
        // clearing the armed windows just keeps the state honest.
        failoverResumeDebounce.clear()
        // Same for a spray deferral still counting down (#275): its runnable
        // re-checks `running` too, but removing the callbacks stops the timers
        // from keeping this instance's closures alive until they fire.
        synchronized(pendingSprayDeferrals) {
            pendingSprayDeferrals.values.forEach(relayMainHandler::removeCallbacks)
            pendingSprayDeferrals.clear()
        }
        synchronized(gatedDigests) { gatedDigests.clear() }
        // FA3: stop accepting new storeExecutor work only after every producer
        // that could submit some is already stopped above (relaySync?.stopPush()
        // clears the push client's hintsProvider and cancels any pending reconnect;
        // cancelDigestMaintenance() removes the only other recurring source).
        // shutdown() (not shutdownNow()) -- graceful: whatever task is already
        // running (e.g. an in-flight digest send) finishes normally instead of
        // being interrupted mid-write; this MeshService instance is done either
        // way, so there is nothing to await synchronously here.
        storeExecutor.shutdown()
        LanTransportDiagnostics.unregisterProbeRequester()
        lanHealthTracker.clear()
        RelaySyncEvents.unregister()
        stopMeshRoles()
        MeshRouter.unregisterCentral()
        MeshRouter.unregisterPeripheral()
        MeshRouter.unregisterLan()
        ChatViewEvents.unregister()
        // stop() above tears down connections without per-address disconnect
        // callbacks, so clear the router's mappings wholesale.
        MeshRouter.reset()
        SprayPolicy.reset()
        MeshConnectivityStatus.clear()
        // Belt to the stopForeground brace above, and the last word in this
        // teardown: stopForeground only removes a notification while the
        // service still *is* foreground, so it cannot undo a plain notify()
        // that landed after the service left that state -- and there is a real
        // window for one. stopSelf() does not destroy the service inline; it
        // posts, so ACTION_STOP's stopForeground/stopSelf pair (see
        // onStartCommand) and this method are two separate main-looper turns,
        // with both broadcast receivers still registered in between and
        // `running` still true, which is exactly what the refresh guard keys
        // on. An A2DP or Bluetooth-state broadcast delivered in that gap
        // re-posts the ongoing notification, and nothing afterwards takes it
        // down. cancel() is unconditional, so the invariant holds against any
        // notify() during teardown rather than only the ones known today.
        // Safe against a restart: a service component's onDestroy always
        // completes before a new instance's onCreate, so this can never cancel
        // a successor's notification.
        runCatching {
            getSystemService(NotificationManager::class.java)?.cancel(NOTIFICATION_ID)
        }
        super.onDestroy()
    }

    /**
     * Restart hardening: [GossipState.seenIds] is an
     * in-memory dedupe set that does not survive a process restart (see its
     * KDoc), while [store] is durable. Without this, a cold app start forgets
     * every `msg_id` we ever authored, so a mule handing one of our own
     * envelopes back to us (Hook A/B just made that routine) would fail to
     * open it -- sealed to the recipient, not us -- and get misclassified as
     * foreign traffic worth carrying. Harmless (the relay and this store both
     * dedupe by `msg_id` too) but wasteful, so every persisted `msg_id` we
     * authored -- our own outbound queue across every 1:1 chat and group,
     * plus whatever we're currently muling for others -- is re-seeded here.
     *
     * FA3: runs on [storeExecutor] (full outbound-envelope scans for every
     * contact and group, plus carried ids), dispatched from
     * [onStartCommand] without blocking the mesh roles that follow it -- see
     * the ordering note at that call site for why racing the seed against
     * real inbound traffic is safe: the paragraph above already establishes
     * that a not-yet-seeded `msg_id` is a harmless, one-time waste, never a
     * correctness or dedupe problem.
     */
    private fun seedSeenIdsFromOwnHistory(identity: Identity) {
        assertOffMainThreadForStore("seedSeenIdsFromOwnHistory")
        try {
            for (contact in store.listContacts()) {
                for (envelope in store.outboundEnvelopesAfter(contact.userId, identity.userId, 0uL)) {
                    GossipState.seenIds.record(envelope.msgId)
                }
            }
            for (group in store.listGroups()) {
                for (envelope in store.outboundEnvelopesAfter(group.id, identity.userId, 0uL)) {
                    GossipState.seenIds.record(envelope.msgId)
                }
            }
            for (msgId in store.carriedMsgIds(SEEN_ID_SEED_CARRIED_LIMIT)) {
                GossipState.seenIds.record(msgId)
            }
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to seed seenIds from own history: ${e.message}")
        }
    }

    private fun startMeshRoles() {
        if (meshRolesRunning) return
        peripheral.start()
        central.start()
        meshRolesRunning = true
        refreshRuntimeState()
        refreshForegroundNotification()
    }

    private fun stopMeshRoles() {
        if (!meshRolesRunning) return
        peripheral.stop()
        central.stop()
        meshRolesRunning = false
        // BLE stop tears links down without per-address disconnect callbacks.
        // Preserve authenticated LAN routes, which remain usable.
        MeshRouter.resetBle()
        MeshConnectivityStatus.refreshNearbyRoutes()
        refreshRuntimeState()
        refreshForegroundNotification()
    }

    /**
     * Tears the BLE roles down and stands them back up. Used when Bluetooth is
     * toggled back on: the stale scanner/advertiser/GATT-server handles from
     * before the toggle are dead, and [BlePeripheral.start]/[BleCentral.start]
     * are idempotent on their own handles, so a plain [startMeshRoles] would
     * see them as "already running" and never rebuild. The [stopMeshRoles] here
     * nulls those handles first so the following start creates fresh ones.
     */
    private fun restartMeshRoles() {
        stopMeshRoles()
        startMeshRoles()
    }

    /** Whether the Bluetooth adapter is present and on (BLE can actually run). */
    private fun isBluetoothOn(): Boolean = bluetoothManager.adapter?.isEnabled == true

    /**
     * Publishes the honest runtime state to [MeshRuntimeStatus]: STOPPED when
     * the service isn't running, NO_BLUETOOTH when it is but the adapter is off
     * (BLE roles can't carry anything), ACTIVE when the roles are up, else
     * STARTING. Called wherever [running], [meshRolesRunning], or the adapter
     * state changes.
     */
    private fun refreshRuntimeState() {
        when {
            !running -> MeshRuntimeStatus.markStopped()
            !isBluetoothOn() -> MeshRuntimeStatus.markNoBluetooth()
            meshRolesRunning -> MeshRuntimeStatus.markActive()
            else -> MeshRuntimeStatus.markStarting()
        }
    }

    private fun registerBluetoothAudioReceiver() {
        if (bluetoothAudioReceiverRegistered) return
        val filter = IntentFilter(BluetoothA2dp.ACTION_CONNECTION_STATE_CHANGED)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(bluetoothAudioReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("DEPRECATION")
            registerReceiver(bluetoothAudioReceiver, filter)
        }
        bluetoothAudioReceiverRegistered = true
    }

    private fun unregisterBluetoothAudioReceiver() {
        if (!bluetoothAudioReceiverRegistered) return
        unregisterReceiver(bluetoothAudioReceiver)
        bluetoothAudioReceiverRegistered = false
    }

    private fun registerBluetoothStateReceiver() {
        if (bluetoothStateReceiverRegistered) return
        val filter = IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(bluetoothStateReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("DEPRECATION")
            registerReceiver(bluetoothStateReceiver, filter)
        }
        bluetoothStateReceiverRegistered = true
    }

    private fun unregisterBluetoothStateReceiver() {
        if (!bluetoothStateReceiverRegistered) return
        unregisterReceiver(bluetoothStateReceiver)
        bluetoothStateReceiverRegistered = false
    }

    /**
     * T15 phase 2: start or stop the internet-less Wi‑Fi association hold to
     * match [WifiHoldPolicy] -- held while the mesh is up and no VPN owns the
     * default route, released otherwise. Idempotent; safe to call on every
     * connectivity change.
     */
    private fun refreshWifiHold() {
        if (!running) {
            wifiHold.stop()
            return
        }
        if (WifiHoldPolicy.shouldHold(relaySync?.isDefaultVpn() == true)) wifiHold.start() else wifiHold.stop()
    }

    /**
     * T15 phase 3: the held Wi‑Fi association actually dropped. If it happened
     * soon after the mesh came up while cellular was still up, it reads as
     * adaptive connectivity tearing down internet-less Wi‑Fi -- count it, and
     * after it repeats the UI surfaces a "keep Wi‑Fi on" tip. Thresholds in
     * [WifiDropPolicy] are first estimates pending Pixel field tuning.
     */
    private fun onWifiAssociationLost() {
        val cellularUp = relaySync?.hasValidatedInternet() == true
        if (WifiDropPolicy.isPrematureDrop(meshJoinedAtMs, System.currentTimeMillis(), cellularUp)) {
            Log.i(TAG, "Wi‑Fi association dropped early with cellular still up; noting for keep-Wi‑Fi tip")
            WifiTipStore.recordPrematureDrop(this)
        }
    }

    private fun registerScreenStateReceiver() {
        if (screenStateReceiverRegistered) return
        val powerManager = getSystemService(Context.POWER_SERVICE) as? PowerManager
        // One-time snapshot for the initial value; screenStateReceiver keeps
        // it current from here (TODO.md task note: "prefer the receiver").
        screenInteractive = powerManager?.isInteractive ?: true
        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_SCREEN_ON)
            addAction(Intent.ACTION_SCREEN_OFF)
        }
        // ACTION_SCREEN_ON/OFF cannot be declared in the manifest (system
        // broadcasts, restricted since API 26) but registerReceiver here is
        // exactly how every other app observes them; not exported since
        // nothing outside this process should be able to spoof screen state.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(screenStateReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("DEPRECATION")
            registerReceiver(screenStateReceiver, filter)
        }
        screenStateReceiverRegistered = true
    }

    private fun unregisterScreenStateReceiver() {
        if (!screenStateReceiverRegistered) return
        unregisterReceiver(screenStateReceiver)
        screenStateReceiverRegistered = false
    }

    private fun scheduleRadioPowerChecks() {
        relayMainHandler.removeCallbacks(radioPowerRunnable)
        relayMainHandler.postDelayed(radioPowerRunnable, RADIO_POWER_CHECK_INTERVAL_MS)
    }

    private fun cancelRadioPowerChecks() {
        relayMainHandler.removeCallbacks(radioPowerRunnable)
    }

    /**
     * Off-[storeExecutor] refresh of [carryQueueLastGrewAtMs] (see that
     * field's doc for the aggregate-vs-per-recipient caveat), then hops back
     * to the main thread to fold the new value into an [evaluateRadioPower]
     * pass -- mirrors every other [store] read in this class (FA3).
     */
    private fun refreshCarryQueueSignal() {
        runOnStoreExecutor("radio power carry-queue check") {
            assertOffMainThreadForStore("refreshCarryQueueSignal")
            val carried = try {
                store.carriedLen().toLong()
            } catch (e: CoreException) {
                Log.w(TAG, "Failed to read carriedLen for radio power policy: ${e.message}")
                return@runOnStoreExecutor
            }
            relayMainHandler.post {
                // T22: only a *growing* queue means new mail worth spending
                // radio on. A steady or shrinking count is mail already in
                // hand, which used to hold the radio at BALANCED forever.
                // The first observation of this process seeds the baseline
                // without escalating: a queue inherited from a previous run
                // is not news.
                val previous = lastCarriedLen
                if (previous >= 0L && carried > previous) {
                    carryQueueLastGrewAtMs = System.currentTimeMillis()
                }
                lastCarriedLen = carried
                evaluateRadioPower("carry-queue check")
            }
        }
    }

    /**
     * Gathers the current [RadioPowerInputs], asks [radioPowerPolicy] for the
     * duty mode, and pushes it to [central]/[peripheral] -- both setters are
     * idempotent, so this is safe (and expected) to call unconditionally
     * from every link-change callback and [radioPowerRunnable]'s tick. Must
     * run on the main thread: [central]/[peripheral] BLE calls expect it
     * (see their own doc comments), same as [startMeshRoles]/[stopMeshRoles].
     */
    private fun evaluateRadioPower(reason: String) {
        if (!running) return
        val now = System.currentTimeMillis()
        val inputs = RadioPowerInputs(
            screenInteractive = screenInteractive,
            // Logical peers, not raw connected routes: a pre-HELLO link cannot
            // carry a message yet, and a peer's dual BLE roles still count once.
            liveLinkCount = MeshRouter.connectedUserCount(),
            msSinceLastLinkChange = if (lastLinkChangeAtMs == 0L) Long.MAX_VALUE / 2 else now - lastLinkChangeAtMs,
            msSinceCarryQueueGrew =
                if (carryQueueLastGrewAtMs == 0L) Long.MAX_VALUE / 2 else now - carryQueueLastGrewAtMs,
        )
        val mode = radioPowerPolicy.evaluate(inputs, now)
        central.setScanDutyMode(mode)
        peripheral.setAdvertiseDutyMode(mode)
        Log.i(TAG, "evaluateRadioPower ($reason): $inputs -> $mode")
    }

    /** Records a link topology change and re-evaluates duty mode -- called from every BLE/LAN connect/disconnect callback. */
    private fun noteLinkChangeAndReevaluate(reason: String) {
        lastLinkChangeAtMs = System.currentTimeMillis()
        // The BLE callbacks that call this can arrive on a GATT binder
        // thread (BleCentral/BlePeripheral invoke onPeerConnected/
        // onCentralSubscribed etc. inline, not posted to the main looper) --
        // hop to relayMainHandler so evaluateRadioPower's BLE calls happen on
        // the same thread [startMeshRoles]/[stopMeshRoles] already use.
        relayMainHandler.post { evaluateRadioPower(reason) }
    }

    private fun scheduleLanHealth() {
        relayMainHandler.removeCallbacks(lanHealthRunnable)
        relayMainHandler.postDelayed(lanHealthRunnable, LAN_HEALTH_INTERVAL_MS)
    }

    private fun cancelLanHealth() {
        relayMainHandler.removeCallbacks(lanHealthRunnable)
    }

    /**
     * Reacts to Bluetooth (A2DP) audio connect/disconnect. Policy as of
     * 2026-07-09 (was: pause both BLE roles entirely while audio is connected):
     * the mesh now stays up regardless, because pausing it silently killed all
     * messaging whenever earbuds were connected -- an unacceptable trade for a
     * messaging app -- and the relaxed low-power scan/advertise interval plus
     * the BALANCED connection priority (see BleCentral/BlePeripheral) are the
     * actual coexistence mitigation for audio stutter. A2DP state now only
     * drives an informational indicator (foreground notification + in-app
     * banner) so a user knows audio and the mesh are sharing the radio.
     *
     * [A2dpAudioBackoff] is reused purely as a connect/disconnect transition
     * detector here; its Mode names predate this policy change.
     */
    private fun refreshBluetoothAudioStatus(reason: String, observedConnected: Boolean? = null) {
        val connected = observedConnected ?: isA2dpConnected()
        val changedMode = a2dpAudioBackoff.update(connected)
        if (changedMode == null && bluetoothAudioConnected == connected) return

        bluetoothAudioConnected = connected
        MeshRuntimeStatus.setBluetoothAudioConnected(connected)
        Log.i(
            TAG,
            if (connected) {
                "Bluetooth audio connected; keeping mesh running ($reason)"
            } else {
                "Bluetooth audio disconnected ($reason)"
            },
        )
        refreshForegroundNotification()
    }

    private fun isA2dpConnected(): Boolean {
        val adapter = bluetoothManager.adapter ?: return false
        return try {
            bluetoothAudioConnectedFromProfileState(
                adapter.getProfileConnectionState(BluetoothProfile.A2DP),
            ) == true
        } catch (e: SecurityException) {
            Log.w(TAG, "Cannot query A2DP connection state; assuming disconnected (${e.message})")
            false
        }
    }

    private fun recordPeerDisconnected(address: String) {
        // Nothing is reset here. Neither the peer's cadence nor this link's
        // byte allowance: a disconnect is what reconnect churn produces (477
        // of them in 88 minutes was the recorded rate), so clearing either on
        // one would hand the churn back the very bound it defeats. Core keeps
        // both accruing against real time and prunes them on its own schedule.
        SprayPolicy.noteLinkClosed(address)
        val userId = MeshRouter.userIdFor(address) ?: return
        val transport = MeshRouter.transportFor(address) ?: return
        if (store.getContact(userId) == null) return
        recordPeerConnection(userId, transport, PeerConnectionEventKind.DISCONNECTED)
    }

    private fun recordPeerConnection(
        userId: ByteArray,
        transport: MeshRouterState.Transport,
        kind: PeerConnectionEventKind,
    ) {
        val path = when (transport) {
            MeshRouterState.Transport.CENTRAL,
            MeshRouterState.Transport.PERIPHERAL -> PeerConnectionTransport.BLUETOOTH
            MeshRouterState.Transport.LAN -> PeerConnectionTransport.LOCAL_WIFI
        }
        runCatching {
            store.recordPeerConnectionEvent(userId, path, kind, System.currentTimeMillis())
        }.onFailure { error ->
            Log.w(TAG, "Could not record connection history: ${error.message}")
        }
    }

    private fun onCentralPeerConnected(address: String) {
        MeshRouter.onConnected(address, MeshRouterState.Transport.CENTRAL)
        sendHello(address)
        noteLinkChangeAndReevaluate("central peer connected")
    }

    private fun onCentralPeerDisconnected(address: String) {
        val peerUserId = MeshRouter.userIdFor(address)
        val wasSelected = MeshRouter.isSelectedRoute(address)
        recordPeerDisconnected(address)
        MeshRouter.onDisconnected(address)
        groupDigestAnswers.forget(address)
        pendingLanHints.clear(address)
        MeshConnectivityStatus.refreshNearbyRoutes()
        if (wasSelected) peerUserId?.let(::scheduleFailoverResume)
        noteLinkChangeAndReevaluate("central peer disconnected")
    }

    private fun onPeripheralCentralSubscribed(address: String) {
        MeshRouter.onConnected(address, MeshRouterState.Transport.PERIPHERAL)
        sendHello(address)
        noteLinkChangeAndReevaluate("peripheral central subscribed")
    }

    private fun onPeripheralCentralDisconnected(address: String) {
        val peerUserId = MeshRouter.userIdFor(address)
        val wasSelected = MeshRouter.isSelectedRoute(address)
        recordPeerDisconnected(address)
        MeshRouter.onDisconnected(address)
        groupDigestAnswers.forget(address)
        pendingLanHints.clear(address)
        MeshConnectivityStatus.refreshNearbyRoutes()
        if (wasSelected) peerUserId?.let(::scheduleFailoverResume)
        noteLinkChangeAndReevaluate("peripheral central disconnected")
    }

    private fun onLanPeerAuthenticated(
        address: String,
        userId: ByteArray,
        endpoint: LanManualEndpoint?,
        networkId: String?,
    ) {
        val previouslySelectedAddress = MeshRouter.routeFor(userId)?.second
        MeshRouter.onConnected(address, MeshRouterState.Transport.LAN)
        noteLinkChangeAndReevaluate("LAN peer authenticated")
        notePossibleIdentityClone(userId)
        if (!MeshRouter.onHello(address, userId)) {
            Log.w(TAG, "Authenticated LAN link could not be registered")
            return
        }
        val contact = store.getContact(userId)
        if (contact != null) {
            // An authenticated LAN link is the strongest possible evidence
            // this contact shares a LAN with us, so it also refreshes the
            // capability recency the automatic-scan gate reads.
            LanCapabilityStore.markSupported(this, userId)
            refreshLanCapableContacts()
            recordPeerConnection(
                userId,
                MeshRouterState.Transport.LAN,
                PeerConnectionEventKind.CONNECTED,
            )
        }
        val peerName = contact?.name ?: "Accepted friend"
        // A completed Noise handshake is the proof a hint never had, so the
        // address is filed as authenticated -- promoting whatever unproven
        // entry was already there. That, and only that, lets it survive a
        // later load on a routed LAN where this phone cannot see itself on
        // the peer's subnet. It mirrors [shouldRetainLanReconnectTarget]:
        // an address that answered is evidence, a claim about one is not.
        endpoint?.let {
            lanEndpointCache.save(networkId, userId, it, LanEndpointProvenance.AUTHENTICATED)
        }
        LanTransportDiagnostics.authenticated(address, peerName)
        Log.i(TAG, "Secure LAN link active with $peerName")
        sendHello(address)
        if (MeshRouter.routeFor(userId)?.second != previouslySelectedAddress) {
            // LAN authentication itself installs the identity mapping. Do not
            // wait for the peer's wire HELLO (or the periodic digest tick) to
            // continue bulk sync on this newly preferred route.
            resumeLogicalPeerSync(userId)
        }
        val currentTransport = lanTransport
        val eagerHint = authenticatedLanEndpointHint(
            contact = contact,
            hint = currentTransport?.currentEndpointHint(),
            networkId = currentTransport?.currentNetworkId(),
        )
        val ownIdentity = identity
        if (eagerHint != null && ownIdentity != null) {
            LanEndpointSender.queueToContact(
                this,
                store,
                ownIdentity,
                eagerHint.contact,
                eagerHint.hint,
                eagerHint.networkId,
            )
        }
        MeshConnectivityStatus.refreshNearbyRoutes()
    }

    private fun onLanPeerDisconnected(address: String) {
        val peerUserId = MeshRouter.userIdFor(address)
        val wasSelected = MeshRouter.isSelectedRoute(address)
        recordPeerDisconnected(address)
        MeshRouter.onDisconnected(address)
        groupDigestAnswers.forget(address)
        lanHealthTracker.remove(address)
        LanTransportDiagnostics.disconnected(address)
        MeshConnectivityStatus.refreshNearbyRoutes()
        if (wasSelected) peerUserId?.let(::scheduleFailoverResume)
        noteLinkChangeAndReevaluate("LAN peer disconnected")
    }

    /**
     * Failover path into [resumeLogicalPeerSync], delayed and coalesced per
     * logical peer.
     *
     * Running the resume synchronously inside the disconnect callback (which
     * is what the three `onDisconnected` handlers above used to do) is wrong
     * when several links die in one radio event: the first callback picks
     * whatever route is *currently* elected -- often a sibling link to the same
     * phone whose own `STATE_DISCONNECTED` is still ~100ms out -- and
     * immediately queues a multi-KB carry drain plus a digest onto it. The
     * peripheral rejects the notification, the link is torn down as a send
     * failure, and the sync has to happen again on the route that actually
     * survived. Waiting one [FailoverResumeDebounce] window lets the rest of
     * the burst land first, so the resume runs once, against the real
     * survivor.
     *
     * The timer lives on [relayMainHandler] but the work does not: the resume
     * touches [store] (via `drainCarriedEnvelopesTo`/[sendDigestTo]) and so
     * hops to [storeExecutor], the same shape as [digestMaintenanceRunnable].
     * Promotion callers ([onLanPeerAuthenticated], [handleHello]) still call
     * [resumeLogicalPeerSync] directly and immediately -- a promotion means a
     * link just came *up*, so there is no dying sibling to wait for.
     *
     * The window is measured on [SystemClock.elapsedRealtime], the same
     * monotonic clock `postDelayed` counts down on: on the wall clock, an NTP
     * correction landing mid-burst would expire the window early and produce
     * the second fan-out this exists to prevent.
     *
     * This is also the coalescing entry point [scheduleDeferredSpray] re-enters
     * through when a peripheral-side spray cooldown lapses (#275), so a
     * deferral landing on top of a live failover burst produces one resume
     * rather than two.
     */
    private fun scheduleFailoverResume(peerUserId: ByteArray) {
        val key = UserIdHex.encode(peerUserId)
        val arm = failoverResumeDebounce.request(key, SystemClock.elapsedRealtime()) ?: return
        relayMainHandler.postDelayed({
            // Cleared before the work runs, so a disconnect arriving while the
            // resume is in flight arms a fresh window instead of being
            // swallowed by this one. The token scopes that to *this* window: a
            // window armed in the meantime keeps its own timer.
            failoverResumeDebounce.fired(key, arm.token)
            if (!running) return@postDelayed
            runOnStoreExecutor("failover resume") { resumeLogicalPeerSync(peerUserId) }
        }, arm.delayMs)
    }

    /** Immediately continue sync after either promotion or failover instead
     * of waiting for the next periodic digest.
     *
     * This is where the peripheral spray cooldown (#275) is read, rather than
     * at the frame handlers that arm it, because this one function is the
     * chokepoint every full burst goes through: a LAN drop failing over onto an
     * inbound BLE address, a route promotion, an election flip inside
     * [handleHello], and the cooldown's own deferred re-entry all land here.
     * Checking at the entry points instead left every one of those free to spray
     * the exact address the cooldown was protecting -- including a re-entry
     * whose link had failed again while the deferral was counting down.
     */
    private fun resumeLogicalPeerSync(peerUserId: ByteArray) {
        val identity = identity ?: return
        val (_, address) = MeshRouter.routeFor(peerUserId) ?: return
        val deferralMs = peripheralSyncSprayDeferralMs(address)
        if (deferralMs > 0L) {
            Log.i(
                TAG,
                "Holding the resume burst for $address for ${deferralMs}ms " +
                    "after a notify-reject teardown on this address",
            )
            scheduleDeferredSpray(peerUserId, deferralMs)
            return
        }
        // Cadence gate (#280). This is the reconnect-churn path: a link dying
        // and being re-elected 5 times a minute used to buy 5 full bursts.
        // First contact is not this path -- [handleHello] owns that -- so a
        // denial here only ever delays a repeat.
        val gate = SprayPolicy.maySpray(peerUserId, address, CoreSprayTrigger.RECONNECT)
        if (!gate.allow) {
            Log.i(TAG, "Holding the resume burst for $address: ${gate.reason} (retry in ${gate.retryAfterMs}ms)")
            rearmGatedSpray(peerUserId, gate)
            return
        }
        Log.i(TAG, "Logical peer selected $address; resuming carry and digest sync")
        // Our own digest is one small frame and it goes FIRST, ahead of every
        // bulk lane below. Core's exchange window opens when the frame is
        // enqueued, which is the only moment this shell can observe; queueing a
        // full carried drain ahead of it would hold it in the GATT FIFO for
        // ~10s on a slow link, and the peer's answering DIGEST would then land
        // after the window it was supposed to arrive inside. The ordering is
        // load-bearing, not cosmetic (#280).
        sendDigestTo(address, peerUserId, identity)
        // A digest this peer sent while its window was open is answered next:
        // its carried-copy confirmations retire envelopes the drain below would
        // otherwise re-offer, which is the spray the cooldown exists to reduce.
        takeGatedDigest(peerUserId)?.let { gated ->
            respondToDigest(address, peerUserId, gated.entries, gated.recentMsgIds, identity, gate, gated.peerAuthenticated)
        }
        val drained = envelopeProcessor?.drainCarriedEnvelopesTo(address, peerUserId, gate.carriedBudgetBytes) ?: 0L
        SprayPolicy.noteBytesQueued(address, drained)
    }

    /**
     * Re-arm a burst the cadence gate held back, when core says the wait is
     * short enough to be worth a timer.
     *
     * Nothing is dropped either way: past that horizon the ordinary 3-5 minute
     * maintenance tick is the cheaper way back, and it always comes. The
     * threshold is core's ([SprayPolicy.retryArmMaxMs]) so neither shell owns
     * it. Re-entry is [scheduleDeferredSpray], the same one-per-peer coalescing
     * path the post-reject cooldown uses, so a gate denial landing on top of a
     * live failover burst still produces one resume rather than two.
     */
    private fun rearmGatedSpray(peerUserId: ByteArray, gate: CoreSprayGate) {
        if (!gate.retryWorthArming || gate.retryAfterMs <= 0L) return
        scheduleDeferredSpray(peerUserId, gate.retryAfterMs)
    }

    /**
     * A peer DIGEST held by the spray cooldown; see [gatedDigests].
     *
     * [peerAuthenticated] is captured at ARRIVAL (CARRY-02) and replayed
     * unchanged so a BLE-sourced digest can never be treated as authenticated
     * merely because it is answered on a later-elected LAN link.
     */
    private class GatedDigest(
        val entries: List<DigestEntry>,
        val recentMsgIds: List<ByteArray>,
        val peerAuthenticated: Boolean,
    )

    private fun stashGatedDigest(
        peerUserId: ByteArray,
        entries: List<DigestEntry>,
        recentMsgIds: List<ByteArray>,
        peerAuthenticated: Boolean,
    ) {
        synchronized(gatedDigests) {
            gatedDigests[UserIdHex.encode(peerUserId)] = GatedDigest(entries, recentMsgIds, peerAuthenticated)
        }
    }

    private fun takeGatedDigest(peerUserId: ByteArray): GatedDigest? =
        synchronized(gatedDigests) { gatedDigests.remove(UserIdHex.encode(peerUserId)) }

    private fun onLanNetworkReady(hint: Frame.LanEndpoint, networkId: String?) {
        val frame = encodeLanEndpointFrame(hint) ?: return
        for (route in MeshRouter.selectedIdentifiedRoutes()) {
            if (route.transport == MeshRouterState.Transport.LAN) continue
            if (store.getContact(route.userId) == null) continue
            MeshRouter.sendToAddress(route.address, frame)
        }
        for (contact in store.listContacts()) {
            // This phone's own address on the network it just joined is what
            // lets the cache throw out an unproven entry that belongs to some
            // other subnet -- the entries shipped builds filed, each of which
            // costs a connect timeout on every join until it does.
            lanEndpointCache.load(networkId, contact.userId, localHost = hint.host)?.let { endpoint ->
                lanTransport?.connectCached(endpoint, contact.userId)
            }
        }
        val ownIdentity = identity
        if (ownIdentity != null) {
            LanEndpointSender.queueToAllCapableContacts(
                this,
                store,
                ownIdentity,
                hint,
                networkId,
            )
        }
    }

    private fun encodeLanEndpointFrame(hint: Frame.LanEndpoint): ByteArray? =
        try {
            encodeLanEndpoint(hint.instanceToken, hint.host, hint.port)
        } catch (error: CoreException) {
            Log.w(TAG, "Unable to encode LAN endpoint hint: ${error.message}")
            null
        }

    private fun sendLanEndpointHintTo(address: String) {
        val transport = MeshRouter.transportFor(address)
        if (
            transport != MeshRouterState.Transport.CENTRAL &&
            transport != MeshRouterState.Transport.PERIPHERAL
        ) {
            return
        }
        val hint = lanTransport?.currentEndpointHint() ?: return
        val frame = encodeLanEndpointFrame(hint) ?: return
        MeshRouter.sendToAddress(address, frame)
    }

    /** Sends our HELLO (DESIGN.md §5.2) as the first frame on a link that just became usable. */
    private fun sendHello(address: String) {
        val ownUserId = identity?.userId ?: return
        MeshRouter.sendToAddress(address, encodeHello(ownUserId))
        // HELLO2 rides right behind the legacy HELLO: capability bits for
        // the hidden-kind spray bound. Pre-HELLO2 builds reject the unknown
        // frame type and drop it without touching the link.
        MeshRouter.sendToAddress(address, encodeHello2(ownUserId, coreOwnCapabilities()))
    }

    private fun onFrameReceived(address: String, frame: ByteArray) {
        val identity = this.identity ?: run {
            Log.w(TAG, "Frame from $address arrived before identity was loaded; dropping")
            return
        }
        val parsed = try {
            parseFrame(frame)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping unparseable frame from $address: ${e.message}")
            return
        }
        when (parsed) {
            is Frame.Hello -> handleHello(address, parsed.userId, identity)
            is Frame.Hello2 -> {
                notePossibleIdentityClone(parsed.userId)
                MeshRouter.onHello2(address, parsed.userId, parsed.capabilities)
            }
            is Frame.Envelope -> envelopeProcessor?.processInboundEnvelope(address, parsed, identity)
            is Frame.Digest -> handleDigest(address, parsed.chatId, parsed.entries, parsed.recentMsgIds, identity)
            is Frame.LanEndpoint -> handleLanEndpointHint(address, parsed)
            is Frame.TransportProbe -> handleTransportProbe(address, parsed)
        }
    }

    private fun handleLanEndpointHint(address: String, hint: Frame.LanEndpoint) {
        if (MeshRouter.transportFor(address) == MeshRouterState.Transport.LAN) return
        val peerUserId = MeshRouter.userIdFor(address) ?: run {
            // The frame-reordering (or notify-congestion) race this log
            // message used to describe permanently as a drop: HELLO hasn't
            // registered this address's userId yet. Hold the hint instead --
            // handleHello replays it the moment this address does HELLO, and
            // onCentralPeerDisconnected/onPeripheralCentralDisconnected clear
            // it if the link dies first.
            Log.i(TAG, "Holding LAN endpoint hint from $address until HELLO")
            pendingLanHints.stash(address, hint)
            return
        }
        if (store.getContact(peerUserId) == null) {
            Log.i(TAG, "Ignoring LAN endpoint hint from an unrecognized peer")
            return
        }
        LanCapabilityStore.markSupported(this, peerUserId)
        refreshLanCapableContacts()
        val localHint = lanTransport?.currentEndpointHint()
        val networkId = lanTransport?.currentNetworkId()
        val ownIdentity = identity
        val contact = store.getContact(peerUserId)
        if (
            localHint != null &&
            networkId != null &&
            ownIdentity != null &&
            contact != null
        ) {
            LanEndpointSender.queueToContact(
                this,
                store,
                ownIdentity,
                contact,
                localHint,
                networkId,
            )
        }
        if (
            MeshRouter.identifiedRoutes().any {
                it.transport == MeshRouterState.Transport.LAN &&
                    it.userId.contentEquals(peerUserId)
            }
        ) {
            return
        }
        lanTransport?.connectToHint(hint, peerUserId)
    }

    private fun handleTransportProbe(address: String, probe: Frame.TransportProbe) {
        if (MeshRouter.transportFor(address) != MeshRouterState.Transport.LAN) return
        if (probe.response) {
            lanHealthTracker.response(address, probe.nonce, System.currentTimeMillis())
                ?.let(LanTransportDiagnostics::probeSucceeded)
        } else {
            MeshRouter.sendToAddress(
                address,
                encodeTransportProbe(probe.nonce, response = true),
            )
        }
    }

    private fun requestManualLanProbe(): String? {
        val route = MeshRouter.identifiedRoutes()
            .firstOrNull { it.transport == MeshRouterState.Transport.LAN }
            ?: return "No secure local Wi-Fi link is active"
        return when (val decision = nextLanHealthDecision(route.address)) {
            is LanHealthTracker.Decision.Send -> {
                LanTransportDiagnostics.probeStarted()
                MeshRouter.sendToAddress(
                    route.address,
                    encodeTransportProbe(decision.nonce, response = false),
                )
                null
            }
            LanHealthTracker.Decision.Wait -> "A LAN connection test is already running"
            LanHealthTracker.Decision.Close -> {
                lanTransport?.closeLink(route.address)
                "The stale LAN link was closed; CruiseMesh will reconnect"
            }
        }
    }

    /**
     * The LAN transport's automatic-scan gate: how many contacts that have
     * recently demonstrated LAN support still have no authenticated LAN link.
     * One connected family member must not stop discovery of the rest, but a
     * contact who is ashore (or simply hasn't been on a shared LAN for a
     * fortnight) must not keep the subnet sweep running on battery forever --
     * see [lanCapabilityMotivatesScan].
     *
     * Runs on the LAN transport's main handler, so it only touches the
     * in-memory router state and the [lanCapableContacts] cache.
     */
    private fun countUnlinkedCapableContacts(): Int {
        val capable = lanCapableContacts
        if (capable.isEmpty()) return 0
        val nowMs = System.currentTimeMillis()
        val linked = MeshRouter.identifiedRoutes()
            .asSequence()
            .filter { it.transport == MeshRouterState.Transport.LAN }
            .mapTo(mutableSetOf()) { UserIdHex.encode(it.userId) }
        return capable.count { (userIdHex, lastSupportedAtMs) ->
            userIdHex !in linked && lanCapabilityMotivatesScan(lastSupportedAtMs, nowMs)
        }
    }

    /**
     * Rebuilds [lanCapableContacts] off the main thread. Called whenever a
     * peer demonstrates LAN support and on the periodic LAN health tick, so
     * a deleted or blocked contact drops out of the sweep gate without any
     * extra plumbing from the screens that delete or block.
     */
    private fun refreshLanCapableContacts() {
        runOnStoreExecutor("lan capability cache") {
            val blocked = store.listBlockedUsers().map(UserIdHex::encode).toSet()
            lanCapableContacts = store.listContacts()
                .asSequence()
                .mapNotNull { contact ->
                    val userIdHex = UserIdHex.encode(contact.userId)
                    if (userIdHex in blocked) return@mapNotNull null
                    LanCapabilityStore.lastSupportedAtMs(this, contact.userId)
                        ?.let { userIdHex to it }
                }
                .toMap()
        }
    }

    private fun checkLanHealth() {
        refreshLanCapableContacts()
        for (route in MeshRouter.identifiedRoutes()) {
            if (route.transport != MeshRouterState.Transport.LAN) continue
            when (val decision = nextLanHealthDecision(route.address)) {
                is LanHealthTracker.Decision.Send -> MeshRouter.sendToAddress(
                    route.address,
                    encodeTransportProbe(decision.nonce, response = false),
                )
                LanHealthTracker.Decision.Wait -> Unit
                LanHealthTracker.Decision.Close -> {
                    LanTransportDiagnostics.probeFailed(
                        "Encrypted LAN heartbeat timed out; reconnecting",
                    )
                    lanTransport?.closeLink(route.address)
                }
            }
        }
    }

    private fun nextLanHealthDecision(address: String): LanHealthTracker.Decision =
        lanHealthTracker.next(
            address = address,
            nowMs = System.currentTimeMillis(),
            nonce = lanProbeNonce.incrementAndGet().toULong(),
        )

    /**
     * HELLO handling (DESIGN.md §5.2 handshake). Records the address->userId
     * mapping, then kicks off the real digest sync (DESIGN.md §7.3). Every
     * peer, contact or stranger, gets a digest now because the advertised
     * `msg_id` set ([MessageStore.coreDigestAdvertisedMsgIds]) is useful to
     * both: it suppresses blind re-spray of foreign mule traffic on
     * reconnect, and (DTN D2 mule-drain-confirm, DTN_TODOS.md §3.2) it
     * doubles as our proof-of-receipt to anyone muling something FOR us --
     * the advertised set includes not just what we're still carrying for
     * others but also what we've recently consumed or authored ourselves,
     * which is exactly the signal [MessageStore.coreConfirmCarriedDeliveries]
     * on the mule's side (called from [sprayDigestPlanTo]) acts on. A known
     * contact additionally gets the per-sender lamport digest for the 1:1
     * chat, i.e. "here's what I have from myself, contiguously, through
     * lamport N per sender." That's the wire-chatId convention from the
     * class KDoc applied to DIGEST frames: `chatId` here is OUR OWN userId,
     * and `entries` is [MessageStore.chatDigest] keyed by the *local* chat
     * (the contact's userId), because locally that's how this 1:1 chat's
     * history is stored. The peer's [handleDigest] uses the matching digest
     * we sent it (from a prior HELLO) the same way to send us what we're
     * missing -- see that method for the receiving half of this exchange.
     * This replaces the earlier naive stand-in that just resent our entire
     * outgoing history on every reconnect.
     *
     * An unrecognized userId still means "not a friend (yet)" for sealed 1:1
     * chat: `entries` is empty, because we have no local chat history keyed to
     * a stranger. But the digest is still worth sending for the carried
     * `msg_id` suppression above.
     */
    /** WPT clone guard: two live devices presenting the same identity. */
    private fun notePossibleIdentityClone(userId: ByteArray) {
        val own = identity?.userId ?: return
        if (!userId.contentEquals(own)) return
        runCatching { store.recordIdentityCloneWarning(userId, System.currentTimeMillis()) }
        ChatEvents.notifyChatChanged(userId)
    }

    private fun handleHello(address: String, userId: ByteArray, identity: Identity) {
        // Register the address->userId mapping before anything else --
        // including the log line below -- to shrink the window for the
        // benign digest-before-HELLO race (see class KDoc / HANDOFF known
        // issue #1): a DIGEST for this same link, delivered on a different
        // binder thread, can otherwise reach handleDigest's
        // MeshRouter.userIdFor(address) lookup before this registration is
        // visible.
        val previouslySelectedAddress = MeshRouter.routeFor(userId)?.second
        MeshRouter.setLocalUserId(identity.userId)
        notePossibleIdentityClone(userId)
        if (!MeshRouter.onHello(address, userId)) {
            Log.w(TAG, "Dropping HELLO that conflicts with the authenticated identity for $address")
            return
        }
        MeshConnectivityStatus.refreshNearbyRoutes()
        MeshConnectivityStatus.mergeLastSeen(UserIdHex.encode(userId), System.currentTimeMillis())
        if (store.getContact(userId) != null) {
            MeshRouter.transportFor(address)?.let { transport ->
                recordPeerConnection(userId, transport, PeerConnectionEventKind.CONNECTED)
            }
        }
        Log.i(TAG, "HELLO from $address: userId=${UserIdHex.encode(userId)}")

        // A LAN endpoint hint that arrived on this address before this HELLO
        // (see handleLanEndpointHint) is now resolvable -- replay it through
        // the normal path instead of leaving the same-Wi-Fi introduction
        // lost for the rest of the connection.
        pendingLanHints.take(address)?.let { hint ->
            Log.i(TAG, "Replaying held LAN endpoint hint from $address")
            handleLanEndpointHint(address, hint)
        }

        val selectedAddress = MeshRouter.routeFor(userId)?.second
        if (selectedAddress != null &&
            selectedAddress != previouslySelectedAddress &&
            selectedAddress != address
        ) {
            // Installing our identity can flip election to an already-HELLO'd
            // inverse BLE role. Continue there even though this HELLO arrived
            // on the now-superseded link.
            resumeLogicalPeerSync(userId)
        }

        if (!MeshRouter.isSelectedRoute(address)) {
            Log.i(
                TAG,
                "HELLO route $address retained for control/failover; bulk sync uses the elected logical-peer route",
            )
            return
        }

        // Post-reject cooldown (#275): this link was torn down moments ago
        // because our own notifications to it were failing, and the burst
        // below is what was failing. The connection is welcome back
        // immediately -- see PeripheralSprayCooldown -- but the multi-KB half
        // of the exchange waits out the window rather than re-running the
        // thing that just broke the link. This handler runs its own drain and
        // digest rather than going through [resumeLogicalPeerSync], so it needs
        // its own read of the window; [handleDigest] gates the other outbound
        // half of the same exchange, and any one of the three left ungated
        // leaves the reconnect loop unbraked.
        val syncDeferralMs = peripheralSyncSprayDeferralMs(address)

        // Cadence gate (#280). This handler claims FIRST_CONTACT because a
        // HELLO is what a fresh encounter looks like from here -- two phones
        // meeting and beginning to sync must never be delayed. Core does not
        // take the claim on trust: it downgrades it to reconnect churn from
        // its own record of this peer, which is what makes 498 connects in 88
        // minutes cost 498 map lookups instead of 498 bursts.
        val gate = SprayPolicy.maySpray(userId, address, CoreSprayTrigger.FIRST_CONTACT)

        val contact = store.getContact(userId)
        if (contact == null) {
            Log.i(TAG, "HELLO from unrecognized userId=${UserIdHex.encode(userId)}; sending carry-suppression digest only")
        } else {
            // One small frame, and the fastest way off this radio entirely if
            // the peer turns out to share our Wi-Fi -- it is not part of the
            // burst either brake holds back, so it goes out before both of
            // them are read.
            sendLanEndpointHintTo(address)
        }

        if (!gate.allow) {
            Log.i(TAG, "Holding the HELLO burst for $address: ${gate.reason} (retry in ${gate.retryAfterMs}ms)")
            rearmGatedSpray(userId, gate)
            return
        }

        if (syncDeferralMs > 0L) {
            Log.i(
                TAG,
                "Holding carry drain and digest for $address for ${syncDeferralMs}ms " +
                    "after a notify-reject teardown on this address",
            )
            scheduleDeferredSpray(userId, syncDeferralMs)
            return
        }

        // Digest first: it is one small frame, and core's exchange window is
        // measured from the moment it is enqueued. Draining the carry queue
        // ahead of it would put up to a full carried budget into this link's
        // single FIFO first, so the frame would not reach the radio for ~10s
        // and the peer's answer would arrive after the window had shut (#280).
        sendDigestTo(address, userId, identity)

        // Then hand off anything we're muling for this peer (DESIGN.md §5.3
        // carry queue). This runs for *any* peer, contact or not: we carry
        // foreign envelopes for strangers too, and a stranger to us may still
        // be the intended recipient of something we picked up. Its bytes are
        // charged to the link because no spray plan accounts for them.
        val drained = envelopeProcessor?.drainCarriedEnvelopesTo(address, userId, gate.carriedBudgetBytes) ?: 0L
        SprayPolicy.noteBytesQueued(address, drained)
    }

    /**
     * The peripheral role's post-notify-reject brake, or 0 when this address
     * isn't an inbound BLE link at all. Only the peripheral role can reject its
     * own notifications this way: as a central we write rather than notify, and
     * LAN has no such failure mode.
     */
    private fun peripheralSyncSprayDeferralMs(address: String): Long {
        if (MeshRouter.transportFor(address) != MeshRouterState.Transport.PERIPHERAL) return 0L
        return peripheral.syncSprayDeferralMs(address)
    }

    /**
     * Re-arms a burst the cooldown held back, so a suppressed carry drain,
     * digest and digest *response* are delayed rather than lost. Nothing else
     * re-runs any of the three on a link that stays up: the DTN carry drain has
     * no periodic tick at all, and the receipts and backlog a peer's digest
     * triggers wait on that peer's own 3-5 min maintenance pass.
     *
     * It lands in [scheduleFailoverResume] rather than calling
     * [resumeLogicalPeerSync] directly so it composes with the #269 debounce
     * instead of racing it: if the link died again during the window, that
     * disconnect has already armed a resume for this peer and this deferral is
     * absorbed into it, so the peer gets one burst rather than two overlapping
     * ones. Re-electing the route at fire time also means a peer that has since
     * moved to a better route (LAN, or the outbound BLE half) is served there.
     *
     * At most one deferral is pending per logical peer. Without that, every
     * gated frame inside one window -- a HELLO and the DIGEST that answers it,
     * or a peer that re-HELLOs on its own retry path -- would post its own
     * timer, and because they fire at different milliseconds the debounce
     * cannot collapse them: its window has closed again between each pair. The
     * result would be N staggered multi-KB bursts ~300ms apart on the one link
     * the cooldown is protecting, which is the overlapping fan-out #269 removed.
     * Replacing the pending timer (rather than keeping the first) is what makes
     * the deferral track the newest cooldown arming instead of firing early.
     */
    private fun scheduleDeferredSpray(peerUserId: ByteArray, delayMs: Long) {
        val key = UserIdHex.encode(peerUserId)
        var pending: Runnable? = null
        val runnable = Runnable {
            synchronized(pendingSprayDeferrals) {
                // Only retire the map entry if it is still *this* timer: a newer
                // deferral armed in the meantime owns the key now.
                if (pendingSprayDeferrals[key] === pending) pendingSprayDeferrals.remove(key)
            }
            if (!running) return@Runnable
            scheduleFailoverResume(peerUserId)
        }
        pending = runnable
        synchronized(pendingSprayDeferrals) {
            pendingSprayDeferrals.put(key, runnable)?.let(relayMainHandler::removeCallbacks)
        }
        relayMainHandler.postDelayed(runnable, delayMs)
    }

    /**
     * Encode and send the §7.3 digest for `address` (per-sender lamports for a
     * known contact, or a carry-suppression digest for a stranger) and tell
     * [SprayPolicy] it went, so the cadence gate and the periodic re-digest
     * (D8) both measure from the same event.
     *
     * Callers gate first. This function does not: it used to *write* a
     * timestamp that only the maintenance tick read, which is precisely why
     * two event-driven callers could spray unthrottled (#280). Recording here
     * also opens core's exchange window, so the peer's answering DIGEST --
     * which our own digest is what provokes -- is not then refused by the gate
     * that just let us speak.
     */
    private fun sendDigestTo(address: String, userId: ByteArray, identity: Identity) {
        val digestEntries = store.getContact(userId)?.let { store.chatDigest(it.userId) } ?: emptyList()
        val digestFrame = try {
            encodeDigest(identity.userId, digestEntries, store.coreDigestAdvertisedMsgIds())
        } catch (error: CoreException) {
            Log.w(TAG, "Could not encode DIGEST for $address", error)
            return
        }
        MeshRouter.sendToAddress(address, digestFrame)
        SprayPolicy.noteDigestSent(userId, address)
        sendGroupDigestsTo(address, userId, identity)
    }

    /**
     * One DIGEST per group we share with [userId], keyed by the group id so
     * a new peer can answer with only the missing envelopes. Old clients
     * drop these via [digestIsExpectedChatId].
     */
    private fun sendGroupDigestsTo(address: String, userId: ByteArray, identity: Identity) {
        val advertised = try {
            store.coreDigestAdvertisedMsgIds()
        } catch (_: CoreException) {
            emptyList()
        }
        for (group in store.listGroups()) {
            if (!group.memberUserIds.any { it.contentEquals(userId) }) continue
            if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) continue
            val entries = try {
                store.chatDigest(group.id)
            } catch (_: CoreException) {
                emptyList()
            }
            val digestFrame = try {
                encodeDigest(group.id, entries, advertised)
            } catch (error: CoreException) {
                Log.w(TAG, "Could not encode group DIGEST for $address", error)
                continue
            }
            MeshRouter.sendToAddress(address, digestFrame)
        }
    }

    /**
     * A DIGEST whose chat_id is a group we share with this link peer.
     * Old clients never get here: they drop via [DigestSync.isExpectedChatId].
     * Not stashed on the cadence gate — that slot is the 1:1 digest.
     */
    private fun handleGroupDigest(
        address: String,
        chatId: ByteArray,
        entries: List<DigestEntry>,
        peerUserId: ByteArray?,
        identity: Identity,
    ) {
        if (peerUserId == null) {
            Log.w(TAG, "Dropping DIGEST from $address: chatId doesn't match this link's HELLO (or no HELLO seen yet)")
            return
        }
        val group = store.getGroup(chatId)
        if (group == null || !digestIsSharedGroup(chatId, peerUserId, identity.userId, group)) {
            Log.w(TAG, "Dropping DIGEST from $address: chatId doesn't match this link's HELLO (or no HELLO seen yet)")
            return
        }
        val gate = SprayPolicy.maySpray(peerUserId, address, CoreSprayTrigger.PEER_DIGEST)
        if (!gate.allow) {
            Log.i(TAG, "Skipping group DIGEST for $address: ${gate.reason}")
            return
        }
        val peerHasThrough = DigestSync.throughLamportForSelf(entries, identity.userId)
        var queuedBytes = resendGroupOutboundToPeer(address, peerUserId, identity, peerHasThrough, group.id)
        val contact = store.getContact(peerUserId)
        if (contact != null) {
            queuedBytes += envelopeProcessor?.syncGroupReceiptsToPeer(identity, contact, address) ?: 0L
        }
        SprayPolicy.noteBytesQueued(address, queuedBytes)
        groupDigestAnswers.note(address, group.id)
    }

    private fun scheduleDigestMaintenance() {
        relayMainHandler.removeCallbacks(digestMaintenanceRunnable)
        relayMainHandler.postDelayed(digestMaintenanceRunnable, DIGEST_MAINTENANCE_INTERVAL_MS)
    }

    private fun cancelDigestMaintenance() {
        relayMainHandler.removeCallbacks(digestMaintenanceRunnable)
    }

    /**
     * D8: re-run the digest exchange on links that have stayed up past their
     * jittered 3-5 min interval, so a message/receipt that landed after the
     * connect-time digest still converges without a reconnect. Digests are
     * idempotent, so this is safe to over-call.
     *
     * FA3: runs on [storeExecutor] (via [digestMaintenanceRunnable]), not
     * [relayMainHandler]'s looper -- [sendDigestTo]'s
     * [MeshRouter.sendToAddress] call is safe to make from here without
     * posting back: it's the identical dispatch [processInboundEnvelope]
     * already performs from the four concurrent receive-path threads.
     */
    private fun checkDigestMaintenance() {
        assertOffMainThreadForStore("checkDigestMaintenance")
        val identity = this.identity ?: return
        if (!running) return
        val routes = MeshRouter.selectedIdentifiedRoutes()
        val now = SprayPolicy.nowMs()
        for (route in routes) {
            // Core still owns the jittered 3-5 minute window; what is new is
            // that a link whose sprays keep producing no receipt progress
            // waits longer, and that this tick and the event-driven callers
            // now read the same record instead of one writing and the other
            // reading. No bookkeeping is retained here: core prunes its own.
            val gate = SprayPolicy.maySpray(route.userId, route.address, CoreSprayTrigger.MAINTENANCE, now)
            if (gate.allow) {
                sendDigestTo(route.address, route.userId, identity)
            }
        }
    }

    /**
     * DIGEST handling (DESIGN.md §7.3): the peer just told us both
     * (a) per-sender contiguous lamports for the 1:1 chat, and
     * (b) the exact carried `msg_id`s it already knows, so a mule doesn't
     * blindly resend them on every reconnect. [DigestSync.isExpectedChatId]
     * checks the wire-chatId sanity condition from the class KDoc -- the
     * digest's `chatId` must equal the userId [MeshRouter] learned for this
     * address via its HELLO. A mismatch, or a digest before any HELLO on
     * this link, means the frame is out of order, so it's logged and dropped
     * rather than acted on.
     *
     * We act on two digest entries, in §7.3's order:
     *
     * 1. The entry for the PEER'S own userId tells us how far their authored
     *    stream exists contiguously from their point of view, which is the
     *    upper bound for the delivered/read receipts we owe them. Those
     *    receipts are re-sent first from the store's persisted outgoing
     *    receipt watermarks, which closes the standalone-receipt retry gap.
     * 2. The entry for OUR OWN userId ([DigestSync.throughLamportForSelf]) is
     *    the peer reporting what of *our* authored history it's missing, so
     *    we resend those messages oldest-first after the receipts.
     *
     * Entries about any other senders are still ignored here -- that is
     * future group traffic rather than this 1:1 chat's retry path. Mule
     * traffic is instead keyed by the digest's exact carried `msg_id` set.
     *
     * Security note (see also this class's KDoc and the core's
     * `protocol.rs` module docs): a DIGEST, like a HELLO, is unauthenticated
     * plaintext link chatter -- there is no signature over it. A lying peer
     * can therefore only ever cause us to (a) resend
     * already-delivered messages, which is harmless because
     * [MessageStore.insertMessage] is idempotent on their end, or (b)
     * withhold sending on this one link if it falsely claims to already
     * have everything, which is harmless because we still have the message
     * locally and the next honest sync (a reconnect, or a different link to
     * the same peer) resends it. It can never cause disclosure -- the
     * resent content is still a sealed envelope only the real recipient can
     * open -- or forgery, since nothing a DIGEST says is ever written to
     * our own store.
     */
    private fun handleDigest(
        address: String,
        chatId: ByteArray,
        entries: List<DigestEntry>,
        recentMsgIds: List<ByteArray>,
        identity: Identity,
    ) {
        val peerUserId = MeshRouter.userIdFor(address)
        if (!DigestSync.isExpectedChatId(chatId, peerUserId)) {
            handleGroupDigest(address, chatId, entries, peerUserId, identity)
            return
        }

        val resolvedPeerUserId = peerUserId!!

        // CARRY-02: the authentication of a carried-delivery confirmation must
        // bind to the transport the digest ARRIVED on, not the link we happen
        // to answer on. `recentMsgIds` (the peer's advertised known-ids) is
        // captured here, at arrival, and carried unchanged through any stash
        // and replay; deriving the flag later from the elected route would let
        // a digest that arrived over unauthenticated BLE be answered on a
        // freshly-elected LAN link and have its advertised ids laundered into
        // an authenticated removal. A LAN link is registered only after a
        // completed Noise handshake whose static key matched an accepted
        // contact ([onLanPeerAuthenticated]); a BLE link is not.
        val peerAuthenticated =
            MeshRouter.transportFor(address) == MeshRouterState.Transport.LAN

        // Post-reject cooldown (#275), the other half of the one in
        // [handleHello]. Everything in [respondToDigest] is outbound on the same
        // notify path the cooldown was armed for, and it is the *larger* half of
        // the burst: receipts, every 1:1 message the peer's watermark says it is
        // missing, every group envelope we authored (from lamport 0 -- there are
        // no group digests yet), and the carry-queue spray. Gating only the
        // HELLO side would not brake the reconnect loop at all: our own HELLO is
        // still sent -- it must be, or the link is useless for anything -- and
        // the peer answers a HELLO with its DIGEST, which lands right here.
        //
        // The digest is held, not discarded: this is the only link path that
        // sends those receipts and that backlog, and the peer receiving our own
        // digest does not make it send another, so discarding would stall both
        // until the peer's next 3-5 min maintenance tick -- on exactly the link
        // that just recovered, and with a stuck receipt watermark being a bug
        // this project has already shipped once (#241).
        val syncDeferralMs = peripheralSyncSprayDeferralMs(address)
        if (syncDeferralMs > 0L) {
            Log.i(
                TAG,
                "Holding the digest response for $address for ${syncDeferralMs}ms " +
                    "after a notify-reject teardown on this address",
            )
            stashGatedDigest(resolvedPeerUserId, entries, recentMsgIds, peerAuthenticated)
            scheduleDeferredSpray(resolvedPeerUserId, syncDeferralMs)
            return
        }

        // Cadence gate (#280). This is the larger outbound half of the
        // exchange, so leaving it ungated would brake nothing. It is normally
        // allowed: our own just-sent digest opened core's exchange window, and
        // this digest is the answer to it. What it refuses is an unprovoked
        // digest arriving from a peer reconnecting every few seconds.
        //
        // A refused digest is held, not discarded, for the same reason the
        // post-reject cooldown holds one: this is the only path that sends the
        // receipts we owe and the backlog the peer's watermark asks for, and
        // #241 is what a stuck receipt watermark costs.
        val gate = SprayPolicy.maySpray(resolvedPeerUserId, address, CoreSprayTrigger.PEER_DIGEST)
        if (!gate.allow) {
            Log.i(
                TAG,
                "Holding the digest response for $address: ${gate.reason} (retry in ${gate.retryAfterMs}ms)",
            )
            stashGatedDigest(resolvedPeerUserId, entries, recentMsgIds, peerAuthenticated)
            rearmGatedSpray(resolvedPeerUserId, gate)
            return
        }

        respondToDigest(address, resolvedPeerUserId, entries, recentMsgIds, identity, gate, peerAuthenticated)
    }

    /**
     * The outbound half of [handleDigest], split out so a digest held by the
     * spray cooldown can be replayed unchanged once the window lapses (see
     * [gatedDigests]).
     *
     * [address] is passed rather than re-derived: on the replay path the elected
     * route may have moved since the digest arrived, and what the peer told us
     * about its own state is true whichever link we answer on.
     *
     * [peerAuthenticated] is likewise passed, not re-derived: it must reflect
     * the transport the digest ARRIVED on (CARRY-02), which on the replay path
     * may differ from [address]'s current transport.
     */
    private fun respondToDigest(
        address: String,
        resolvedPeerUserId: ByteArray,
        entries: List<DigestEntry>,
        recentMsgIds: List<ByteArray>,
        identity: Identity,
        gate: CoreSprayGate,
        peerAuthenticated: Boolean,
    ) {
        // Everything queued here that is not part of the spray plan -- the
        // receipt repair pass, the per-missing-message re-send loop and the
        // group catch-up -- is counted and charged against this link's burst
        // allowance at the end. They are the encounter's LARGEST lanes, and
        // while they went uncharged a second DIGEST arriving inside the
        // exchange window could re-run all of them against an untouched
        // allowance (#280).
        var queuedBytes = 0L
        val contact = store.getContact(resolvedPeerUserId)
        if (contact != null) {
            queuedBytes += envelopeProcessor?.syncReceiptsFirst(identity, contact, address) ?: 0L
            queuedBytes += envelopeProcessor?.syncGroupReceiptsToPeer(identity, contact, address) ?: 0L
            val peerHasThrough = DigestSync.throughLamportForSelf(entries, identity.userId)
            val queuedByLamport = store
                .outboundEnvelopesAfter(contact.userId, identity.userId, peerHasThrough)
                .associateBy { it.lamport }
            // Same once-per-session bound as the core spray plan: a peer
            // without CAP_ACKS_HIDDEN_KINDS never advances its DELIVERED
            // watermark past hidden kinds, so this direct re-offer would
            // repeat them on every digest for the full expiry.
            val gateHidden = !MeshRouter.peerAcksHiddenKinds(address)
            val alreadyOffered = if (gateHidden) {
                MeshRouter.hiddenOfferedFor(address).mapTo(mutableSetOf(), UserIdHex::encode)
            } else {
                mutableSetOf()
            }
            val newlyOffered = mutableListOf<ByteArray>()
            val missing = store.messagesAfter(contact.userId, identity.userId, peerHasThrough)
            for (message in missing) {
                val outbound = queuedByLamport[message.lamport] ?: backfillOutboundAuthoredEnvelope(identity, contact, message)
                if (outbound != null) {
                    if (gateHidden && coreIsHiddenSprayKind(outbound.kind)) {
                        if (UserIdHex.encode(outbound.msgId) in alreadyOffered) continue
                        newlyOffered += outbound.msgId
                    }
                    queuedBytes += sendStoredOutboundEnvelope(address, outbound)
                }
            }
            MeshRouter.recordHiddenOffered(address, newlyOffered)
            // New peers advertise per-group watermarks. Old clients never do,
            // so they still get the lamport-0 catch-up (inserts are idempotent).
            // Skip only groups this peer already got an answered digest for.
            queuedBytes += resendGroupOutboundToPeer(
                address,
                resolvedPeerUserId,
                identity,
                0uL,
                skipAnsweredGroups = true,
            )
        }
        // Charged before the plan is built, so `sprayDigestPlanTo`'s own
        // admission sees a link allowance that already reflects what this
        // encounter has queued.
        SprayPolicy.noteBytesQueued(address, queuedBytes)
        envelopeProcessor?.sprayDigestPlanTo(address, resolvedPeerUserId, recentMsgIds, identity, gate, peerAuthenticated)
        if (contact == null) {
            Log.i(TAG, "DIGEST from unrecognized userId=${UserIdHex.encode(resolvedPeerUserId)}; sprayed carry queue only")
        }
    }

    /**
     * Best-effort group catch-up: send our sealed group traffic (and any
     * queued pairwise invites addressed to this peer) for groups the peer
     * belongs to, starting after [afterLamport]. A group digest supplies the
     * watermark; the 1:1 fallback still passes 0 for old clients.
     *
     * Returns the sealed bytes queued so the caller can charge the link's
     * burst allowance (#280).
     */
    private fun resendGroupOutboundToPeer(
        address: String,
        peerUserId: ByteArray,
        identity: Identity,
        afterLamport: ULong,
        onlyGroupId: ByteArray? = null,
        skipAnsweredGroups: Boolean = false,
    ): Long {
        var queuedBytes = 0L
        for (group in store.listGroups()) {
            if (onlyGroupId != null && !group.id.contentEquals(onlyGroupId)) continue
            if (skipAnsweredGroups && groupDigestAnswers.answered(address, group.id)) continue
            if (!group.memberUserIds.any { it.contentEquals(peerUserId) }) continue
            if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) continue
            val envelopes = store.outboundEnvelopesAfter(group.id, identity.userId, afterLamport)
            for (envelope in envelopes) {
                // Pairwise invites are only useful to their intended recipient;
                // group-sealed text has recipientUserId = group.id.
                if (envelope.kind == KIND_GROUP_INVITE &&
                    !envelope.recipientUserId.contentEquals(peerUserId)
                ) {
                    continue
                }
                queuedBytes += sendStoredOutboundEnvelope(address, envelope)
            }
        }
        return queuedBytes
    }

    /**
     * Re-seals one locally authored chat-stream message the outbound queue no
     * longer holds a sealed copy of, so this peer's digest can be answered for
     * that lamport.
     *
     * Core decides what happens to the rebuilt envelope beyond being returned
     * here (`outbound_retirement.rs`, #283). A row can be missing because it
     * predates the outbound-envelope table, because a delivered receipt retired
     * it, or because a newer generation of a snapshot kind superseded it; only
     * the first belongs back in the queue, and only core knows which case this
     * is. This function never assumes: it asks, sends what comes back, and
     * leaves the queue to the store. The returned `msgId` is the message's own
     * persisted id, so [GossipState.seenIds] and the `alreadyOffered` bound in
     * [respondToDigest] both keep recognising a retransmission as one.
     */
    private fun backfillOutboundAuthoredEnvelope(
        identity: Identity,
        contact: Contact,
        message: StoredMessage,
    ): OutboundEnvelope? {
        val authored = try {
            store.backfillPairwiseEnvelope(identity, contact, message, null)
        } catch (error: CoreException) {
            Log.w(TAG, "Unable to backfill legacy authored envelope", error)
            return null
        }
        GossipState.seenIds.record(authored.envelope.msgId)
        return authored.envelope
    }

    /** Sends one previously persisted outbound envelope on the exact link [address]. */
    /**
     * Queues one stored outbound envelope at [address] and reports the sealed
     * bytes that cost, so the caller can charge them against the link's burst
     * allowance. These re-sends are not part of any spray plan, and until they
     * were charged the per-link cap bounded the plan rather than the link
     * (#280).
     */
    private fun sendStoredOutboundEnvelope(address: String, envelope: OutboundEnvelope): Long {
        if (!MeshRouter.sendToAddress(address, encodeOutboundEnvelopeFrame(envelope))) return 0L
        return envelope.sealed.size.toLong()
    }

    private fun hasRequiredPermissions(): Boolean {
        // minSdk is 31 (S), so BLUETOOTH_SCAN/ADVERTISE/CONNECT are always required.
        val permissions = listOf(
            Manifest.permission.BLUETOOTH_SCAN,
            Manifest.permission.BLUETOOTH_ADVERTISE,
            Manifest.permission.BLUETOOTH_CONNECT,
        )
        return permissions.all {
            ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED
        }
    }

    private fun buildNotification(): Notification {
        // minSdk is 31, so notification channels always exist (added in API 26).
        val channel = NotificationChannel(
            NOTIFICATION_CHANNEL_ID,
            getString(R.string.notification_mesh_channel_name),
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            setShowBadge(false)
        }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        val contentIntent = PendingIntent.getActivity(
            this,
            OPEN_APP_REQUEST_CODE,
            Intent(this, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
            },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val stopIntent = PendingIntent.getService(
            this,
            STOP_SERVICE_REQUEST_CODE,
            Intent(this, MeshService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val enableBluetoothIntent = PendingIntent.getActivity(
            this,
            ENABLE_BLUETOOTH_REQUEST_CODE,
            Intent(this, MainActivity::class.java).apply {
                action = MainActivity.ACTION_REQUEST_BLUETOOTH_ENABLE
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
            },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val bluetoothOff = !isBluetoothOn()
        return NotificationCompat.Builder(this, NOTIFICATION_CHANNEL_ID)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(
                when {
                    bluetoothOff -> getString(R.string.notification_mesh_paused_bluetooth)
                    bluetoothAudioConnected -> getString(R.string.notification_mesh_relaying_with_audio)
                    else -> getString(R.string.notification_mesh_relaying)
                },
            )
            // FA9: app-owned icon, was android.R.drawable.stat_sys_data_bluetooth.
            .setSmallIcon(R.drawable.ic_notification_mesh)
            .setBadgeIconType(NotificationCompat.BADGE_ICON_NONE)
            .setContentIntent(contentIntent)
            .apply {
                if (bluetoothOff) {
                    addAction(
                        android.R.drawable.stat_sys_data_bluetooth,
                        getString(R.string.ui_turn_on_bluetooth),
                        enableBluetoothIntent,
                    )
                }
            }
            .addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                getString(R.string.notification_stop_cruisemesh),
                stopIntent,
            )
            .setOngoing(true)
            .build()
    }

    /**
     * Updates the ongoing notification in place -- but only while the service
     * is actually live.
     *
     * The guard is the whole point. [onDestroy] removes the notification with
     * `stopForeground(STOP_FOREGROUND_REMOVE)` and *then*, further down the
     * same teardown, calls [stopMeshRoles], which ends with a refresh. Without
     * the guard that refresh re-posted the notification a few milliseconds
     * after it was removed, from a service that was already dying: tapping
     * "Stop CruiseMesh" made the notification blink and come straight back,
     * so the mesh looked like it had refused to stop when it had in fact
     * stopped. Worse, what came back was an ongoing (undismissable)
     * notification with no service behind it, and nothing could then take it
     * down: it arrived by plain notify(), not as a foreground attachment, so
     * neither teardown's stopForeground nor a second Stop tap touched it (that
     * tap's fresh service instance no-ops straight back out -- [stopMeshRoles]
     * early-returns with [meshRolesRunning] false). It survived until the app
     * was next opened and [clearStaleNotification] reconciled it. Reported by
     * a tester 2026-08-02.
     *
     * Same T21 invariant as the [onDestroy] note: the ongoing mesh
     * notification must never outlive the service. Removal has to win over
     * every refresh that teardown itself triggers, so the choke point is
     * guarded rather than each caller: [running] is cleared as the first
     * statement of [onDestroy], before anything that can refresh.
     */
    private fun refreshForegroundNotification() {
        if (!running) return
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, buildNotification())
    }

    companion object {
        const val ACTION_STOP = "com.cruisemesh.app.action.STOP_MESH"

        /**
         * T21: clears a mesh notification left behind by a process that was
         * killed outright.
         *
         * [onDestroy] removes the notification on every teardown it gets to
         * run for, but a SIGKILL (the low-memory killer, a force-stop, a
         * pause) never reaches it, so the ongoing notification can outlive
         * the service and tell the user the mesh is up when nothing is
         * running. Measured on a family phone: two notifications posted,
         * zero services, for about two hours.
         *
         * Called from the activity on launch, before the mesh is (re)started,
         * and safe either way -- if the mesh comes up immediately afterwards
         * it posts a fresh notification through [startForeground].
         */
        fun clearStaleNotification(context: Context) {
            if (MeshRuntimeStatus.state.value != MeshRuntimeState.STOPPED) return
            runCatching {
                context.getSystemService(NotificationManager::class.java)
                    ?.cancel(NOTIFICATION_ID)
            }
        }

        /** Permissions MeshService needs before it will start its BLE roles. */
        fun requiredPermissions(): Array<String> =
            // minSdk is 31 (S), so the Bluetooth trio is always required.
            arrayOf(
                Manifest.permission.BLUETOOTH_SCAN,
                Manifest.permission.BLUETOOTH_ADVERTISE,
                Manifest.permission.BLUETOOTH_CONNECT,
            )
    }
}
