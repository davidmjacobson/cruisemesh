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
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.identity.IdentityStore
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.DigestEntry
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.encodeDigest
import uniffi.cruisemesh_core.shouldRedigest
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
private const val LAN_HEALTH_INTERVAL_MS = 30_000L
// D8: how often to check whether a long-lived link is due for a re-digest. The
// actual 3-5 min jittered gate lives in core `shouldRedigest`; this only sets
// the polling granularity.
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
     * T2's `carriedLen()` is the closest thing to "holding mail for someone
     * we can't currently reach" the store exposes today -- see
     * [RadioPowerPolicy]'s class doc for why this is an aggregate
     * approximation, not a per-recipient one. Refreshed off [storeExecutor]
     * by [refreshCarryQueueSignal] on every [radioPowerRunnable] tick.
     */
    @Volatile private var carryQueueHasUnlinkedMail: Boolean = false

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
     * where [carryQueueHasUnlinkedMail] gets refreshed, since that requires
     * a [storeExecutor] hop (see [refreshCarryQueueSignal]).
     */
    private val radioPowerRunnable = object : Runnable {
        override fun run() {
            refreshCarryQueueSignal()
            if (running) relayMainHandler.postDelayed(this, RADIO_POWER_CHECK_INTERVAL_MS)
        }
    }
    // D8: when this link last ran a digest exchange, keyed by peer address.
    private val lastDigestAtByAddress = java.util.concurrent.ConcurrentHashMap<String, Long>()

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
            MeshStartupPreferences.markExplicitlyStopped(this)
            MeshRuntimeStatus.markStopped()
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }

        // A manual/app start begins a new session. BootReceiver checks the
        // explicit-stop bit before it ever reaches this path.
        MeshStartupPreferences.clearExplicitStop(this)
        startForeground(NOTIFICATION_ID, buildNotification())
        // Debug builds: ensure log capture is running even if the process was
        // revived straight into the service without the UI (no-op in release
        // and idempotent with MainActivity's call).
        DebugFileLog.start(this)
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
        store = AppStore.get(this)
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

                override fun saveLanEndpoint(networkId: String?, userId: ByteArray, endpoint: LanManualEndpoint) =
                    lanEndpointCache.save(networkId, userId, endpoint)

                override fun currentLanNetworkId(): String? = lanTransport?.currentNetworkId()
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
            onNetworkReady = ::onLanNetworkReady,
            onEndpointObserved = { userId, endpoint, networkId ->
                lanEndpointCache.save(networkId, userId, endpoint)
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
        MeshConnectivityStatus.clear()
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
        MeshConnectivityStatus.setNearbyPeers(MeshRouter.helloedUserIds())
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
     * Off-[storeExecutor] refresh of [carryQueueHasUnlinkedMail] (see that
     * field's doc for the aggregate-vs-per-recipient caveat), then hops back
     * to the main thread to fold the new value into an [evaluateRadioPower]
     * pass -- mirrors every other [store] read in this class (FA3).
     */
    private fun refreshCarryQueueSignal() {
        runOnStoreExecutor("radio power carry-queue check") {
            assertOffMainThreadForStore("refreshCarryQueueSignal")
            val hasUnlinkedMail = try {
                store.carriedLen() > 0uL
            } catch (e: CoreException) {
                Log.w(TAG, "Failed to read carriedLen for radio power policy: ${e.message}")
                false
            }
            relayMainHandler.post {
                carryQueueHasUnlinkedMail = hasUnlinkedMail
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
            // identifiedRoutes(), not the raw connected-socket count: a link
            // that hasn't HELLO'd yet can't carry a message yet either, so it
            // shouldn't count as "not lonely" for duty-mode purposes.
            liveLinkCount = MeshRouter.identifiedRoutes().size,
            msSinceLastLinkChange = if (lastLinkChangeAtMs == 0L) Long.MAX_VALUE / 2 else now - lastLinkChangeAtMs,
            carryQueueHasUnlinkedMail = carryQueueHasUnlinkedMail,
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

    private fun onCentralPeerConnected(address: String) {
        MeshRouter.onConnected(address, MeshRouterState.Transport.CENTRAL)
        sendHello(address)
        noteLinkChangeAndReevaluate("central peer connected")
    }

    private fun onCentralPeerDisconnected(address: String) {
        MeshRouter.onDisconnected(address)
        pendingLanHints.clear(address)
        MeshConnectivityStatus.setNearbyPeers(MeshRouter.helloedUserIds())
        noteLinkChangeAndReevaluate("central peer disconnected")
    }

    private fun onPeripheralCentralSubscribed(address: String) {
        MeshRouter.onConnected(address, MeshRouterState.Transport.PERIPHERAL)
        sendHello(address)
        noteLinkChangeAndReevaluate("peripheral central subscribed")
    }

    private fun onPeripheralCentralDisconnected(address: String) {
        MeshRouter.onDisconnected(address)
        pendingLanHints.clear(address)
        MeshConnectivityStatus.setNearbyPeers(MeshRouter.helloedUserIds())
        noteLinkChangeAndReevaluate("peripheral central disconnected")
    }

    private fun onLanPeerAuthenticated(
        address: String,
        userId: ByteArray,
        endpoint: LanManualEndpoint?,
        networkId: String?,
    ) {
        MeshRouter.onConnected(address, MeshRouterState.Transport.LAN)
        noteLinkChangeAndReevaluate("LAN peer authenticated")
        if (!MeshRouter.onHello(address, userId)) {
            Log.w(TAG, "Authenticated LAN link could not be registered")
            return
        }
        val contact = store.getContact(userId)
        val peerName = contact?.name ?: "Accepted friend"
        endpoint?.let { lanEndpointCache.save(networkId, userId, it) }
        LanTransportDiagnostics.authenticated(address, peerName)
        Log.i(TAG, "Secure LAN link active with $peerName")
        sendHello(address)
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
        MeshConnectivityStatus.setNearbyPeers(MeshRouter.helloedUserIds())
    }

    private fun onLanPeerDisconnected(address: String) {
        MeshRouter.onDisconnected(address)
        lanHealthTracker.remove(address)
        LanTransportDiagnostics.disconnected(address)
        MeshConnectivityStatus.setNearbyPeers(MeshRouter.helloedUserIds())
        noteLinkChangeAndReevaluate("LAN peer disconnected")
    }

    private fun onLanNetworkReady(hint: Frame.LanEndpoint, networkId: String?) {
        val frame = encodeLanEndpointFrame(hint) ?: return
        for (route in MeshRouter.identifiedRoutes()) {
            if (route.transport == MeshRouterState.Transport.LAN) continue
            if (store.getContact(route.userId) == null) continue
            MeshRouter.sendToAddress(route.address, frame)
        }
        for (contact in store.listContacts()) {
            lanEndpointCache.load(networkId, contact.userId)?.let { endpoint ->
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
            is Frame.Hello2 -> MeshRouter.onHello2(address, parsed.userId, parsed.capabilities)
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

    private fun checkLanHealth() {
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
    private fun handleHello(address: String, userId: ByteArray, identity: Identity) {
        // Register the address->userId mapping before anything else --
        // including the log line below -- to shrink the window for the
        // benign digest-before-HELLO race (see class KDoc / HANDOFF known
        // issue #1): a DIGEST for this same link, delivered on a different
        // binder thread, can otherwise reach handleDigest's
        // MeshRouter.userIdFor(address) lookup before this registration is
        // visible.
        if (!MeshRouter.onHello(address, userId)) {
            Log.w(TAG, "Dropping HELLO that conflicts with the authenticated identity for $address")
            return
        }
        MeshConnectivityStatus.setNearbyPeers(MeshRouter.helloedUserIds())
        MeshConnectivityStatus.mergeLastSeen(UserIdHex.encode(userId), System.currentTimeMillis())
        Log.i(TAG, "HELLO from $address: userId=${UserIdHex.encode(userId)}")

        // A LAN endpoint hint that arrived on this address before this HELLO
        // (see handleLanEndpointHint) is now resolvable -- replay it through
        // the normal path instead of leaving the same-Wi-Fi introduction
        // lost for the rest of the connection.
        pendingLanHints.take(address)?.let { hint ->
            Log.i(TAG, "Replaying held LAN endpoint hint from $address")
            handleLanEndpointHint(address, hint)
        }

        // Hand off anything we're muling for this peer (DESIGN.md §5.3 carry
        // queue) before the digest sync. This runs for *any* peer, contact or
        // not: we carry foreign envelopes for strangers too, and a stranger to
        // us may still be the intended recipient of something we picked up.
        envelopeProcessor?.drainCarriedEnvelopesTo(address, userId)

        val contact = store.getContact(userId)
        if (contact == null) {
            Log.i(TAG, "HELLO from unrecognized userId=${UserIdHex.encode(userId)}; sending carry-suppression digest only")
        } else {
            sendLanEndpointHintTo(address)
        }
        sendDigestTo(address, userId, identity)
    }

    /**
     * Encode and send the §7.3 digest for `address` (per-sender lamports for a
     * known contact, or a carry-suppression digest for a stranger) and record
     * the time so [checkDigestMaintenance] can re-run it on a long-lived link
     * (D8). Called at HELLO time and on the periodic re-digest tick.
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
        lastDigestAtByAddress[address] = System.currentTimeMillis()
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
        val routes = MeshRouter.identifiedRoutes()
        // Drop bookkeeping for links that have gone away.
        lastDigestAtByAddress.keys.retainAll(routes.map { it.address }.toSet())
        val now = System.currentTimeMillis()
        for (route in routes) {
            val last = lastDigestAtByAddress[route.address] ?: 0L
            if (shouldRedigest(now, last, route.address.hashCode().toLong().toULong())) {
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
            Log.w(TAG, "Dropping DIGEST from $address: chatId doesn't match this link's HELLO (or no HELLO seen yet)")
            return
        }

        val resolvedPeerUserId = peerUserId!!
        val contact = store.getContact(resolvedPeerUserId)
        if (contact != null) {
            envelopeProcessor?.syncReceiptsFirst(identity, contact, address, entries)
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
                    sendStoredOutboundEnvelope(address, outbound)
                }
            }
            MeshRouter.recordHiddenOffered(address, newlyOffered)
            // Group digests are not on the wire yet (1:1 digest only). At family
            // scale, re-offer every outbound group envelope we authored for
            // groups this peer is in; their insert is idempotent.
            resendGroupOutboundToPeer(address, resolvedPeerUserId, identity)
        }
        envelopeProcessor?.sprayDigestPlanTo(address, resolvedPeerUserId, recentMsgIds, identity)
        if (contact == null) {
            Log.i(TAG, "DIGEST from unrecognized userId=${UserIdHex.encode(resolvedPeerUserId)}; sprayed carry queue only")
        }
    }

    /**
     * Best-effort group catch-up on reconnect: send our sealed group traffic
     * (and any queued pairwise invites addressed to this peer) for groups the
     * peer belongs to. Without per-group digests we resend from lamport 0;
     * duplicates are safe on the receiver.
     */
    private fun resendGroupOutboundToPeer(address: String, peerUserId: ByteArray, identity: Identity) {
        for (group in store.listGroups()) {
            if (!group.memberUserIds.any { it.contentEquals(peerUserId) }) continue
            if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) continue
            val envelopes = store.outboundEnvelopesAfter(group.id, identity.userId, 0uL)
            for (envelope in envelopes) {
                // Pairwise invites are only useful to their intended recipient;
                // group-sealed text has recipientUserId = group.id.
                if (envelope.kind == KIND_GROUP_INVITE &&
                    !envelope.recipientUserId.contentEquals(peerUserId)
                ) {
                    continue
                }
                sendStoredOutboundEnvelope(address, envelope)
            }
        }
    }

    /**
     * Re-queues an older locally authored chat-stream message that predates
     * the new outbound-envelope table, so reconnect retry and relay upload can
     * use the same persisted-envelope path as newly authored traffic.
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
    private fun sendStoredOutboundEnvelope(address: String, envelope: OutboundEnvelope) {
        MeshRouter.sendToAddress(address, encodeOutboundEnvelopeFrame(envelope))
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
            "CruiseMesh mesh sync",
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
        return NotificationCompat.Builder(this, NOTIFICATION_CHANNEL_ID)
            .setContentTitle("CruiseMesh")
            .setContentText(
                when {
                    !isBluetoothOn() -> "Paused — turn on Bluetooth to sync nearby"
                    bluetoothAudioConnected -> "Relaying messages nearby (Bluetooth audio also connected)"
                    else -> "Relaying messages nearby"
                },
            )
            // FA9: app-owned icon, was android.R.drawable.stat_sys_data_bluetooth.
            .setSmallIcon(R.drawable.ic_notification_mesh)
            .setBadgeIconType(NotificationCompat.BADGE_ICON_NONE)
            .setContentIntent(contentIntent)
            .addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                "Stop CruiseMesh",
                stopIntent,
            )
            .setOngoing(true)
            .build()
    }

    private fun refreshForegroundNotification() {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, buildNotification())
    }

    companion object {
        const val ACTION_STOP = "com.cruisemesh.app.action.STOP_MESH"

        /** Permissions MeshService needs before it will start its BLE roles. */
        fun requiredPermissions(): Array<String> {
            // minSdk is 31 (S), so the Bluetooth trio is always required.
            val base = mutableListOf(
                Manifest.permission.BLUETOOTH_SCAN,
                Manifest.permission.BLUETOOTH_ADVERTISE,
                Manifest.permission.BLUETOOTH_CONNECT,
            )
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                base += Manifest.permission.POST_NOTIFICATIONS
            }
            return base.toTypedArray()
        }
    }
}
