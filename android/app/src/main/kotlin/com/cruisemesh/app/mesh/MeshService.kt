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
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
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
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.friending.ProfileSyncSender
import com.cruisemesh.app.friending.FriendImportEvents
import com.cruisemesh.app.friending.FriendDirectorySender
import com.cruisemesh.app.friending.FriendRequestSender
import com.cruisemesh.app.friending.FriendsOfFriendsStore
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.KIND_REACTION
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.MessageNotifier
import com.cruisemesh.app.relay.RelayClient
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayPushClient
import com.cruisemesh.app.relay.normalizeRelayUrl
import com.cruisemesh.app.relay.RelayImport
import uniffi.cruisemesh_core.CarriedEnvelope
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.ContactDiscoveryPolicy
import uniffi.cruisemesh_core.ContactProvenance
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreInboundGate
import uniffi.cruisemesh_core.CoreRelayEnvelopeDisposition
import uniffi.cruisemesh_core.DigestEntry
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageArrival
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OpenedMessage
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.ReceiptContent
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.applyGroupMetadataUpdate
import uniffi.cruisemesh_core.coreGroupFanoutRows
import uniffi.cruisemesh_core.coreGroupFanoutRowsForCarried
import uniffi.cruisemesh_core.coreInboundGate
import uniffi.cruisemesh_core.coreIsOwnFanoutHint
import uniffi.cruisemesh_core.corePairwiseSenderAuthorized
import uniffi.cruisemesh_core.decodeGroupInviteContent
import uniffi.cruisemesh_core.decodeGroupMetadataUpdate
import uniffi.cruisemesh_core.decodeFriendDirectoryContent
import uniffi.cruisemesh_core.decodeIntroducedFriendRequest
import uniffi.cruisemesh_core.decodeLanEndpointContent
import uniffi.cruisemesh_core.decodeExtendedMessageBody
import uniffi.cruisemesh_core.decodeProfileSyncContent
import uniffi.cruisemesh_core.decodeReceiptContent
import uniffi.cruisemesh_core.defaultExpiry
import uniffi.cruisemesh_core.encodeDigest
import uniffi.cruisemesh_core.shouldRedigest
import uniffi.cruisemesh_core.encodeEnvelopeFrame
import uniffi.cruisemesh_core.encodeHello
import uniffi.cruisemesh_core.encodeLanEndpoint
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.encodeReceiptContent
import uniffi.cruisemesh_core.encodeTransportProbe
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.openGroupMessage
import uniffi.cruisemesh_core.parseFriendCard
import uniffi.cruisemesh_core.openMessage
import uniffi.cruisemesh_core.parseFrame
import uniffi.cruisemesh_core.relayFetchBatchLimit
import uniffi.cruisemesh_core.sealMessage
import uniffi.cruisemesh_core.verifyIntroductionTicket

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

/** `kind` bytes from DESIGN.md §7.1. */
private const val KIND_TEXT: UByte = 1u
private const val KIND_RECEIPT: UByte = 2u
private const val KIND_FRIEND_REQUEST: UByte = 3u
private const val KIND_GROUP_INVITE: UByte = 4u
private const val KIND_PROFILE_SYNC: UByte = 5u
private const val KIND_FRIEND_DIRECTORY: UByte = 6u
private const val KIND_INTRODUCED_FRIEND_REQUEST: UByte = 7u
private const val KIND_LAN_ENDPOINT_HINT: UByte = 8u
private const val KIND_GROUP_METADATA_UPDATE: UByte = 19u

/** `receipt_type` values (DESIGN.md §7.2): delivered = recipient decrypted and stored it, read = recipient viewed the chat. */
private const val RECEIPT_TYPE_DELIVERED: UByte = 1u
private const val RECEIPT_TYPE_READ: UByte = 2u

internal fun bluetoothAudioConnectedFromProfileState(state: Int): Boolean? = when (state) {
    BluetoothProfile.STATE_CONNECTED -> true
    BluetoothProfile.STATE_DISCONNECTED -> false
    else -> null
}

/** DESIGN.md §5.3: hop budget a freshly authored envelope's §6.4 header starts with. Mirrors core's `DEFAULT_HOP_TTL`. */
private const val DEFAULT_HOP_TTL: UByte = 7u

private const val MS_PER_DAY: Long = 24L * 60 * 60 * 1000

/**
 * How many recent day-numbers to hash a peer's UserID against when matching
 * carried envelopes (DESIGN.md §5.3 carry queue, §6.4 `recipient_hint`). A
 * `recipient_hint` is `BLAKE2b-8(UserID || day-number)` where the day-number
 * is the envelope's *creation* day; since envelopes live `DEFAULT_EXPIRY_MS`
 * (7 days) and an unexpired one was created at most 7 days ago, hashing a
 * peer's UserID against today back through 7 days ago covers every day-salt a
 * still-carried envelope for them could have used. Mirrors core's
 * `DEFAULT_EXPIRY_MS` / `MS_PER_DAY`.
 */
private const val CARRY_HINT_DAY_WINDOW: Long = 7
private const val PRESENCE_HINT_DAY_WINDOW: Long = 3

/** DESIGN.md §5.3: the bounded budget (~5 MB) of *foreign* muled envelopes; family (known-recipient) traffic is exempt. */
private const val FOREIGN_CARRY_BUDGET_BYTES: Long = 5L * 1024 * 1024
private const val RELAY_STORE_BATCH_LIMIT: ULong = 128uL
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
 * BLE_1TO1_MULING.md Hook B: bounded per-digest-exchange budget (sealed-byte
 * size) for spraying our own still-undelivered 1:1 outbound envelopes to a
 * non-recipient mule. Same order of magnitude as [FOREIGN_CARRY_BUDGET_BYTES]
 * is generous for storage, but this budget bounds one GATT exchange's worth
 * of traffic, not total storage, so it's much smaller.
 */
private const val OWN_OUTBOUND_SPRAY_BUDGET_BYTES: Long = 256L * 1024

/**
 * BLE_1TO1_MULING.md §6 follow-up: bounded per-digest-exchange budget
 * (sealed-byte size) for spraying our own still-undelivered outgoing receipt
 * envelopes to a mule so it can carry them back toward the original message
 * senders. Receipts are tiny (a fixed cumulative watermark, no message body),
 * so this is far smaller than [OWN_OUTBOUND_SPRAY_BUDGET_BYTES] -- 64 KiB is
 * hundreds of receipts, a backstop against a pathological backlog rather than
 * a normal-case limiter.
 */
private const val OWN_RECEIPT_SPRAY_BUDGET_BYTES: Long = 64L * 1024

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
 * ### The wire `chatId` convention (read this before touching frame handling)
 *
 * Locally, a 1:1 chat is always keyed by "the other party's userId" -- see
 * [com.cruisemesh.app.chat.ChatScreen] and [com.cruisemesh.app.chat.RealMeshSender]. A message I
 * send to contact C is stored under `chatId = C.userId`; a message C sends
 * to me is *also* stored under `chatId = C.userId`, because from my side C
 * is always "the other party," regardless of who authored the message.
 *
 * On the wire, though, [MessageBody.chatId] is set by the SENDER to the
 * SENDER's OWN userId, not the recipient's. That looks backwards until you
 * read it from the receiving side: [handleEnvelope] below checks
 * `body.chatId == opened.senderUserId`, which only makes sense if wire
 * `chatId` names "whoever sent this frame." That value is also exactly what
 * the receiver needs to store the message under locally (their convention:
 * `chatId` = the other party = the sender). So "wire chatId = sender's own
 * userId" is what makes the sender's and receiver's local conventions line
 * up without either side rewriting anything after the fact. The same
 * convention applies to receipts (see [handleIncomingText]'s outgoing
 * receipt): a receipt's wire `chatId` is the *receipt sender's* own userId
 * (i.e. mine, when I'm acking someone else's message), for the identical
 * reason. And it applies to DIGEST frames too (DESIGN.md §7.3, see
 * [handleHello]'s outgoing digest and [handleDigest]'s sanity check): a
 * digest's wire `chatId` is the *digest sender's* own userId, so "does this
 * digest's chatId match what [MeshRouter] learned from this link's HELLO"
 * is exactly the right check for "is this digest about the chat I think it
 * is."
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
    private var relayNetworkCallbackRegistered = false
    private var lanTransport: LanTransport? = null
    private val lanHealthTracker = LanHealthTracker()
    private val lanProbeNonce = AtomicLong(System.nanoTime())

    /** FA5: atomic per-msg_id admission gate across the four concurrent receive-path threads -- see [processInboundEnvelope]. */
    private val inboundAdmission = InboundEnvelopeAdmission()

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
    private val relayPushClient by lazy {
        RelayPushClient(
            relayMainHandler,
            onPush = { requestRelaySync("relay push") },
            onHealthChanged = ::onRelayPushHealthChanged,
        )
    }

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

    /** Health [relayPushClient] reported at the last poll-interval decision; null before the first one. See [onRelayPushHealthChanged]/[relayPollRunnable]. */
    @Volatile private var lastKnownPushHealthy: Boolean? = null

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
            refreshWifiHold()
            updateRelayPushSubscription()
        }

        override fun onLost(network: Network) {
            if (relayBindNetwork == network) relayBindNetwork = null
            if (!hasValidatedInternet()) {
                MeshConnectivityStatus.setRelayHealth(RelayHealth.NoInternet)
            }
            refreshWifiHold()
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
            runOnStoreExecutor("initial relay health (repeat start)") { publishInitialRelayHealth() }
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
        ChatViewEvents.register(::handleChatViewed)
        RelaySyncEvents.register { requestRelaySync("queue changed") }

        running = true
        runOnStoreExecutor("initial relay health") { publishInitialRelayHealth() }
        registerBluetoothAudioReceiver()
        registerBluetoothStateReceiver()
        registerRelayNetworkCallback()
        registerScreenStateReceiver()
        meshJoinedAtMs = System.currentTimeMillis()
        WifiTipStore.refresh(this)
        refreshWifiHold()
        scheduleRelayPolling()
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
        requestRelaySync("service start")
        updateRelayPushSubscription()
        return START_STICKY
    }

    override fun onDestroy() {
        running = false
        lanTransport?.stop()
        lanTransport = null
        MeshRuntimeStatus.markStopped()
        unregisterBluetoothAudioReceiver()
        unregisterBluetoothStateReceiver()
        unregisterRelayNetworkCallback()
        unregisterScreenStateReceiver()
        wifiHold.stop()
        cancelRelayPolling()
        relayPushClient.stop()
        cancelLanHealth()
        cancelDigestMaintenance()
        cancelRadioPowerChecks()
        // FA3: stop accepting new storeExecutor work only after every producer
        // that could submit some is already stopped above (relayPushClient.stop()
        // clears its hintsProvider and cancels any pending reconnect;
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
     * BLE_1TO1_MULING.md §5 restart hardening: [GossipState.seenIds] is an
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

    private fun registerRelayNetworkCallback() {
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

    private fun unregisterRelayNetworkCallback() {
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
    private fun hasValidatedInternet(): Boolean =
        isDefaultValidated() || (!isDefaultVpn() && relayBindNetwork != null)

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
        if (WifiHoldPolicy.shouldHold(isDefaultVpn())) wifiHold.start() else wifiHold.stop()
    }

    /**
     * T15 phase 3: the held Wi‑Fi association actually dropped. If it happened
     * soon after the mesh came up while cellular was still up, it reads as
     * adaptive connectivity tearing down internet-less Wi‑Fi -- count it, and
     * after it repeats the UI surfaces a "keep Wi‑Fi on" tip. Thresholds in
     * [WifiDropPolicy] are first estimates pending Pixel field tuning.
     */
    private fun onWifiAssociationLost() {
        val cellularUp = hasValidatedInternet()
        if (WifiDropPolicy.isPrematureDrop(meshJoinedAtMs, System.currentTimeMillis(), cellularUp)) {
            Log.i(TAG, "Wi‑Fi association dropped early with cellular still up; noting for keep-Wi‑Fi tip")
            WifiTipStore.recordPrematureDrop(this)
        }
    }

    /** FA3: runs on [storeExecutor] -- see the call sites in [onStartCommand]. */
    private fun publishInitialRelayHealth() {
        assertOffMainThreadForStore("publishInitialRelayHealth")
        val contacts = try {
            store.listContacts()
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to inspect contacts for initial relay status: ${e.message}")
            emptyList()
        }
        val configs = distinctRelayConfigs(contacts, RelayConfigStore.load(this))
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
    private fun isDefaultVpn(): Boolean {
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

    private fun scheduleRelayPolling() {
        // Push health is unknown at this point (RelayPushClient hasn't been
        // (re)started for this session yet) -- start at the unhealthy/safety
        // cadence; the first real health report reschedules from there via
        // [onRelayPushHealthChanged].
        lastKnownPushHealthy = null
        relayMainHandler.removeCallbacks(relayPollRunnable)
        relayMainHandler.postDelayed(relayPollRunnable, RadioPowerPolicy.RELAY_POLL_UNHEALTHY_MS)
    }

    private fun cancelRelayPolling() {
        relayMainHandler.removeCallbacks(relayPollRunnable)
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
        relayMainHandler.removeCallbacks(relayPollRunnable)
        relayMainHandler.postDelayed(relayPollRunnable, interval)
    }

    /** [RelayPushClient]'s health-change callback -- see [relayPushClient]'s doc and [RadioPowerPolicy]'s "Relay poll cadence" section. */
    private fun onRelayPushHealthChanged(healthy: Boolean) {
        Log.i(TAG, "Relay push health -> $healthy")
        reschedulePoll(healthy)
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
     * (re)connect from [relayHintsForConfig] (mail addressed to us) and
     * [relayProxyHints] (mail addressed to a contact we can proxy-fetch for,
     * same as [pollRelayMailbox]'s doc) so a newly added contact or group is
     * picked up the next reconnect without this needing its own
     * change-tracking; until then the 60s poll already covers it.
     *
     * FA3: that recomputation used to run synchronously on the main thread
     * inside [RelayPushClient.connect] (whichever thread called [start] or
     * its own delayed reconnect, always [relayMainHandler]'s looper). It's
     * now an async callback -- [RelayPushClient] hands us a completion
     * function instead of expecting a return value, we compute the hints on
     * [storeExecutor] via [computeRelayPushHints], and it resumes connecting
     * once we call back. See [RelayPushClient]'s class doc for the resulting
     * state machine.
     */
    private fun updateRelayPushSubscription() {
        val identity = this.identity
        val config = RelayConfigStore.load(this)
        if (identity == null || config == null || !hasValidatedInternet()) {
            relayPushClient.stop()
            return
        }
        relayPushClient.start(config) { onReady -> computeRelayPushHints(identity, onReady) }
    }

    /**
     * FA3: computes the relay-push hint set on [storeExecutor] and always
     * invokes [onReady] exactly once -- with the hints, with `emptyList()` if
     * the computation throws, or with `emptyList()` if [storeExecutor] has
     * already been shut down ([onDestroy] racing a pending reconnect).
     * [RelayPushClient.connect] depends on hearing back to decide whether to
     * open a socket or back off and retry (empty hints reads as "nothing to
     * subscribe to yet," same as before this fix); silently dropping the
     * callback would strand it never reconnecting.
     */
    private fun computeRelayPushHints(identity: Identity, onReady: (List<ByteArray>) -> Unit) {
        runOnStoreExecutorAlwaysReplying("relay push hint computation", { onReady(emptyList()) }) {
            assertOffMainThreadForStore("relay push hint computation")
            val now = System.currentTimeMillis()
            val hints = try {
                dedupeHints(
                    relayHintsForConfig(identity.userId, now),
                    relayProxyHints(store.listContacts(), identity.userId, now),
                )
            } catch (e: CoreException) {
                Log.w(TAG, "Failed to compute relay push hints: ${e.message}")
                emptyList()
            }
            onReady(hints)
        }
    }

    private fun scheduleLanHealth() {
        relayMainHandler.removeCallbacks(lanHealthRunnable)
        relayMainHandler.postDelayed(lanHealthRunnable, LAN_HEALTH_INTERVAL_MS)
    }

    private fun cancelLanHealth() {
        relayMainHandler.removeCallbacks(lanHealthRunnable)
    }

    private fun requestRelaySync(reason: String) {
        if (!running || identity == null) return
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
                    if (relaySyncPending && running && hasValidatedInternet()) {
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
        val identity = this.identity ?: return
        val now = System.currentTimeMillis()
        store.pruneExpiredOutboundEnvelopes(now)
        store.pruneExpiredOutgoingReceiptEnvelopes(now)
        store.pruneExpiredCarried(now)
        val contacts = store.listContacts()
        val fallbackConfig = RelayConfigStore.load(this)
        // Bind this whole pass to a validated network when the default can't be
        // trusted (associated-but-dead Wi‑Fi, no VPN); otherwise null = use the
        // default (normal networks and VPN tunnels route themselves).
        val network = relayBindTarget()
        backfillRelayOutgoingReceiptEnvelopes(identity, contacts, now)
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
                pollRelayMailbox(config, identity, contacts, fallbackConfig, now, network)
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

    private fun backfillRelayOutgoingReceiptEnvelopes(
        identity: Identity,
        contacts: List<Contact>,
        now: Long,
    ) {
        for (contact in contacts) {
            queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_DELIVERED,
                ackedSenderUserId = contact.userId,
                throughLamport = store.outgoingReceiptThrough(
                    contact.userId,
                    contact.userId,
                    RECEIPT_TYPE_DELIVERED,
                ),
                timestamp = now,
            )
            queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = contact.userId,
                throughLamport = store.outgoingReceiptThrough(
                    contact.userId,
                    contact.userId,
                    RECEIPT_TYPE_READ,
                ),
                timestamp = now,
            )
        }
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
            val contact = contactMatchingHint(contacts, envelope.recipientHint, now)
            if (contact == null) {
                // Group-hinted carried envelope: previously skipped entirely
                // (no contact match). A member mule can now decompose it into
                // per-member fan-out rows (specs/group-relay-durability.md
                // §4.2) so the group's mail reaches internet-only members
                // through this phone's uplink too. No mark-posted concept for
                // carried rows -- re-posts every pass dedupe server-side via
                // the deterministic fan-out ids. Non-member mules still can't
                // recognize the hint and still skip, unchanged.
                val group = groupMatchingHint(envelope.recipientHint, now) ?: continue
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
     * set so they ride the same paginated fetch: [relayHintsForConfig] (mail
     * addressed to us, pairwise or via a group we belong to) and
     * [relayProxyHints] (mail addressed to a *contact*, fetched on their
     * behalf -- relay proxy-polling, see that function's doc for why this is
     * the fix for "a 1:1 message to a WiFi-less recipient never bridges
     * across BLE clusters"). Every fetched envelope still goes through
     * [handleRelayEnvelope] -> [processInboundEnvelope] exactly as before;
     * what's new is that the ack decision now follows the returned
     * [CoreInboundDisposition] via [MessageStore.coreRelayAckIdsWithConsumed]
     * instead of unconditionally acking everything the fetch returned. A
     * proxied envelope comes back as CARRIED, not CONSUMED, so it is
     * deliberately left on the relay -- [MeshService.carryRelayEnvelope]
     * already queued it for BLE delivery to its real recipient, and the
     * relay copy remains the durable fallback until they (or another proxy)
     * fetch and consume it, or it expires. A SEEN envelope this device
     * already consumed as a 1:1 message over BLE/LAN is now also acked
     * (DTN_TODOS.md §3.1) instead of being re-fetched on every pass until
     * expiry -- see [CoreRelayEnvelopeDisposition]'s KDoc for the exact rule.
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
     *  - [relayProxyHints] fetches every contact's hints on every pass, so
     *    its cost scales with contact-list size. Fine for this app's small
     *    family circles; would need a smarter server-side "for this family
     *    token" fan-out if that ever became a large flat social graph.
     */
    private fun pollRelayMailbox(
        config: RelayConfig,
        identity: Identity,
        contacts: List<Contact>,
        fallbackConfig: RelayConfig?,
        now: Long,
        network: Network?,
    ) {
        val selfHints = relayHintsForConfig(identity.userId, now)
        val proxyHints = relayProxyHints(contacts, identity.userId, now)
        val hints = dedupeHints(selfHints, proxyHints)
        if (hints.isEmpty()) return
        var after = 0L
        val fetchBatchLimit = relayFetchBatchLimit().toInt()
        while (running && hasValidatedInternet()) {
            val page = RelayClient.fetchEnvelopes(config, hints, after, fetchBatchLimit, network)
            Log.i(
                TAG,
                "Fetched ${page.envelopes.size} relay envelope(s) from ${config.relayUrl} after=$after next=${page.nextCursor}",
            )
            if (page.envelopes.isEmpty()) return
            val dispositions = ArrayList<CoreRelayEnvelopeDisposition>(page.envelopes.size)
            for (envelope in page.envelopes) {
                val disposition = handleRelayEnvelope(envelope, identity)
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

    /** Mail addressed to us: our own hint, plus every group we belong to (DESIGN.md §6.5). */
    private fun relayHintsForConfig(
        ownUserId: ByteArray,
        now: Long,
    ): List<ByteArray> {
        val hints = recentHintsFor(ownUserId, now).toMutableList()
        // Pull group-addressed mail for every group we belong to (DESIGN.md §6.5).
        for (group in store.listGroups()) {
            if (group.memberUserIds.any { it.contentEquals(ownUserId) }) {
                hints += recentHintsFor(group.id, now)
            }
        }
        return hints
    }

    /**
     * "Proxy" hints for relay proxy-polling: the recent-day `recipient_hint`s
     * for every contact that isn't us, so an internet-connected phone sitting
     * in a BLE-only contact's cluster can also fetch mail addressed to *them*
     * out of the shared family-token relay partition, then carry it over BLE
     * the rest of the way ([MeshService.carryRelayEnvelope]). Without this,
     * only the contact's own (internet-less) hint would ever be polled, and a
     * 1:1 envelope addressed to them would sit unfetched forever.
     *
     * Bounded by family size: this fetches every contact's hints on every
     * poll pass, so cost grows linearly with the contact list. That's fine
     * for the small family circles this app targets; see the scaling TODO on
     * [pollRelayMailbox] if that assumption ever changes.
     */
    private fun relayProxyHints(
        contacts: List<Contact>,
        ownUserId: ByteArray,
        now: Long,
    ): List<ByteArray> {
        val hints = mutableListOf<ByteArray>()
        for (contact in contacts) {
            if (contact.userId.contentEquals(ownUserId)) continue
            hints += recentHintsFor(contact.userId, now)
        }
        return hints
    }

    /**
     * Combines [selfHints] and [proxyHints] into one fetch list, deduping by
     * content (`ByteArray` has reference equality, so a plain `distinct()`
     * would not catch two equal-content hints -- e.g. a contact hint that
     * happens to coincide with a group hint we already fetch for ourselves).
     */
    private fun dedupeHints(selfHints: List<ByteArray>, proxyHints: List<ByteArray>): List<ByteArray> {
        val seen = HashSet<String>(selfHints.size + proxyHints.size)
        val out = ArrayList<ByteArray>(selfHints.size + proxyHints.size)
        for (hint in selfHints + proxyHints) {
            if (seen.add(UserIdHex.encode(hint))) {
                out += hint
            }
        }
        return out
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
        val announce = if (RelayConfigStore.shareOnline(this)) {
            recentPresenceHintsFor(identity.userId, now)
        } else {
            emptyList()
        }
        val query = dedupeHints(
            emptyList(),
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

    private fun contactMatchingHint(contacts: List<Contact>, hint: ByteArray, now: Long): Contact? {
        contacts.firstOrNull { contact ->
            recentHintsFor(contact.userId, now).any { it.contentEquals(hint) }
        }?.let { return it }
        // Group-addressed carries: upload via any member's relay config.
        for (group in store.listGroups()) {
            if (!recentHintsFor(group.id, now).any { it.contentEquals(hint) }) continue
            for (memberId in group.memberUserIds) {
                val contact = contacts.firstOrNull { it.userId.contentEquals(memberId) }
                if (contact != null) return contact
            }
        }
        return null
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
            adapter.getProfileConnectionState(BluetoothProfile.A2DP) == BluetoothProfile.STATE_CONNECTED
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
            is Frame.Envelope -> handleEnvelope(address, parsed, identity)
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
        drainCarriedEnvelopesTo(address, userId)

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
            syncReceiptsFirst(identity, contact, address, entries)
            val peerHasThrough = DigestSync.throughLamportForSelf(entries, identity.userId)
            val queuedByLamport = store
                .outboundEnvelopesAfter(contact.userId, identity.userId, peerHasThrough)
                .associateBy { it.lamport }
            val missing = store.messagesAfter(contact.userId, identity.userId, peerHasThrough)
            for (message in missing) {
                val outbound = queuedByLamport[message.lamport] ?: backfillOutboundAuthoredEnvelope(identity, contact, message)
                if (outbound != null) {
                    sendStoredOutboundEnvelope(address, outbound)
                }
            }
            // Group digests are not on the wire yet (1:1 digest only). At family
            // scale, re-offer every outbound group envelope we authored for
            // groups this peer is in; their insert is idempotent.
            resendGroupOutboundToPeer(address, resolvedPeerUserId, identity)
        }
        sprayDigestPlanTo(address, resolvedPeerUserId, recentMsgIds, identity)
        if (contact == null) {
            Log.i(TAG, "DIGEST from unrecognized userId=${UserIdHex.encode(resolvedPeerUserId)}; sprayed carry queue only")
        }
    }

    /** Executes Rust's complete digest-time mule plan. */
    private fun sprayDigestPlanTo(
        address: String,
        peerUserId: ByteArray,
        peerKnownMsgIds: List<ByteArray>,
        identity: Identity,
    ) {
        val now = System.currentTimeMillis()
        try {
            // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): confirm delivery
            // of anything this digest's advertised `msg_id`s prove the peer
            // already has BEFORE building the spray plan below, so a
            // just-confirmed carried envelope isn't immediately re-sprayed
            // back at the peer who just told us they have it.
            val confirmed = store.coreConfirmCarriedDeliveries(peerUserId, peerKnownMsgIds, now)
            if (confirmed > 0uL) {
                Log.i(TAG, "Confirmed delivery of $confirmed carried envelope(s) to ${UserIdHex.encode(peerUserId)}; dropped our copy")
            }
            val plan = store.coreDigestSprayPlan(
                ownUserId = identity.userId,
                peerUserId = peerUserId,
                peerHints = recentHintsFor(peerUserId, now),
                peerKnownMsgIds = peerKnownMsgIds,
                nowMs = now,
                ownOutboundBudgetBytes = OWN_OUTBOUND_SPRAY_BUDGET_BYTES.toULong(),
                ownReceiptBudgetBytes = OWN_RECEIPT_SPRAY_BUDGET_BYTES.toULong(),
                receiptQueryLimit = RELAY_STORE_BATCH_LIMIT,
            )
            val frames = plan.carriedFrames + plan.ownOutboundFrames + plan.ownReceiptFrames
            val sprayed = frames.count { MeshRouter.sendToAddress(address, it) }
            Log.i(
                TAG,
                "Digest spray to $address sent $sprayed/${frames.size} frame(s) " +
                    "(carried=${plan.carriedFrames.size}, authored=${plan.ownOutboundFrames.size}, receipts=${plan.ownReceiptFrames.size})",
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to build digest spray plan for $address: ${e.message}")
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
     * DESIGN.md §7.3: receipts go first on peer sync because they're the
     * smallest frames and unblock the most UI. The store persists the latest
     * cumulative delivered/read watermarks we owe [contact], so a receipt that
     * couldn't be sent when it was first observed heals on this reconnect.
     *
     * The digest entry for [contact.userId] is "how far the peer says its own
     * authored stream exists contiguously"; receipts acknowledging beyond that
     * point are capped away as nonsensical. In the ordinary case the cap is a
     * no-op, but it makes the foreign digest entry actively meaningful rather
     * than ignored.
     */
    private fun syncReceiptsFirst(
        identity: Identity,
        contact: Contact,
        address: String,
        entries: List<DigestEntry>,
    ) {
        val peerAuthoredThrough = DigestSync.throughLamportForSender(entries, contact.userId)
        if (peerAuthoredThrough == 0uL) return

        val deliveredThrough = minOf(
            store.outgoingReceiptThrough(contact.userId, contact.userId, RECEIPT_TYPE_DELIVERED),
            peerAuthoredThrough,
        )
        if (deliveredThrough > 0uL) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_DELIVERED, contact.userId, deliveredThrough)
        }

        val readThrough = minOf(
            store.outgoingReceiptThrough(contact.userId, contact.userId, RECEIPT_TYPE_READ),
            peerAuthoredThrough,
        )
        if (readThrough > 0uL) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, contact.userId, readThrough)
        }
    }

    /**
     * Envelope handling with §5.3 gossip in front of §6.3 delivery.
     *
     * Every inbound `0x02` frame carries the §6.4 public header, so before
     * touching crypto we run the flooding logic DESIGN.md §5.3 calls for:
     *
     * 1. **Dedupe** on `msg_id` via the shared [GossipState.seenIds]. A
     *    `msg_id` we've already handled (on this or any other link, including
     *    one we ourselves authored -- see [sealEnvelopeFrame]) is dropped
     *    outright: it was already delivered-or-relayed the first time, and the
     *    mesh's redundant links guarantee we'll see popular frames more than
     *    once. This is the single most important line for not melting the
     *    network with a flood.
     * 2. **Expiry**: a carrier drops an envelope past its `expiry`
     *    (DESIGN.md §5.3) rather than delivering or forwarding it. For
     *    freshly authored direct traffic expiry is a week out so this never
     *    fires; it matters for the old muled traffic a future carry queue
     *    (§5.3) will hold.
     * 3. **Open vs relay**: we try to [openMessage]. A sealed box is anonymous
     *    and addressed to exactly one X25519 key (§6.3), so *opening it means
     *    we are the intended recipient* -- deliver locally and do NOT re-flood
     *    (it's home). Failure means it's foreign traffic just passing through,
     *    so [relayForeignEnvelope] floods it onward with a decremented
     *    `hop_ttl`. (A failure could also be a corrupt/garbage envelope; we
     *    can't tell those apart from "not for us" without the key, and relaying
     *    a few bad frames is cheap and TTL-bounded, so we treat both the same.)
     *
     * Delivery itself (decode body, the `chatId == verified sender` sanity
     * check explained in this class's KDoc, kind dispatch) is unchanged --
     * see [deliverOpenedEnvelope].
     *
     * DTN D4 (seen-set poisoning ordering): [GossipState.seenIds] is checked
     * with the non-mutating [uniffi.cruisemesh_core.SeenIds.contains], never
     * [uniffi.cruisemesh_core.SeenIds.checkAndRecord], and only recorded once
     * this envelope reaches a **terminal handled state** -- consumed,
     * carried, or expired-drop -- at each `return` below. Invariant: an
     * envelope whose durable handling failed must be re-presentable; an
     * envelope that was handled (even by deliberate drop) must be deduped.
     * Before this, `checkAndRecord` ran up front, so a later store failure
     * (e.g. disk-full out of [carryForeignEnvelope]) permanently poisoned the
     * `msg_id` even though it was never actually carried or delivered.
     *
     * [processInboundEnvelope] now returns a [CoreInboundDisposition] so
     * [pollRelayMailbox] (the relay path) can decide whether it's safe to ack
     * the envelope; this BLE path has no such concept (a link frame isn't
     * "acked"), so it just ignores the return value.
     */
    private fun handleEnvelope(address: String, envelope: Frame.Envelope, identity: Identity) {
        processInboundEnvelope(address, envelope, identity)
    }

    private fun handleRelayEnvelope(
        envelope: com.cruisemesh.app.relay.RelayFetchedEnvelope,
        identity: Identity,
    ): CoreInboundDisposition {
        Log.i(
            TAG,
            "Handling relay envelope id=${envelope.id} msgId=${UserIdHex.encode(envelope.msgId)} hopTtl=${envelope.hopTtl}",
        )
        return processInboundEnvelope(
            sourceAddress = null,
            envelope = Frame.Envelope(
                msgId = envelope.msgId,
                hopTtl = envelope.hopTtl,
                expiry = envelope.expiryMs,
                recipientHint = envelope.recipientHint,
                sealed = envelope.sealed,
            ),
            identity = identity,
        )
    }

    /**
     * [sourceAddress] doubles as the source discriminant relay proxy-polling
     * needs: `null` means this envelope came FROM the relay
     * ([handleRelayEnvelope]), non-null means it arrived over a live BLE or
     * authenticated same-LAN link ([handleEnvelope]). The two foreign-carry branches below use that to
     * pick [carryRelayEnvelope] (durable, never re-uploaded -- it's already on
     * the relay) vs. the existing [carryForeignEnvelope] (durable-if-family,
     * uploaded to the relay so an internet phone can proxy it onward) for
     * envelopes we can't open ourselves. See [CoreInboundDisposition] for what
     * each return value means to the caller.
     */
    private fun processInboundEnvelope(
        sourceAddress: String?,
        envelope: Frame.Envelope,
        identity: Identity,
    ): CoreInboundDisposition {
        val sourceLabel = sourceAddress ?: "relay"
        // FA5: this function runs concurrently on up to four threads (central-
        // GATT binder, peripheral-GATT binder, LanTransport's
        // connectionExecutor, the relay-sync thread) -- see
        // [InboundEnvelopeAdmission]'s KDoc for the full threading model.
        // Claim this msg_id before touching the seen-set or dispatching
        // anything: a rejected claim means another thread is already
        // mid-flight on this exact msg_id right now (e.g. the same message
        // arriving over BLE and LAN at once), so treat it exactly like an
        // ordinary dedupe instead of double-delivering/double-flooding.
        if (!inboundAdmission.tryBegin(envelope.msgId)) {
            return CoreInboundDisposition.SEEN
        }
        // Every return below must go through this so the admission claim
        // above is always released. `terminal = true` also runs
        // GossipState.seenIds.record for this msg_id -- still under
        // [InboundEnvelopeAdmission]'s lock, so no other thread can re-claim
        // this msg_id between the record landing and the claim releasing.
        fun finishAdmission(disposition: CoreInboundDisposition, terminal: Boolean): CoreInboundDisposition {
            inboundAdmission.finish(envelope.msgId, terminal) { GossipState.seenIds.record(envelope.msgId) }
            return disposition
        }

        // DTN D4: a non-mutating check, not checkAndRecord -- see the KDoc
        // above. `record` (via finishAdmission's terminal=true) is only
        // called once handling below actually reaches a terminal state, so a
        // failure partway through leaves this msg_id re-presentable on the
        // next copy instead of poisoned forever.
        when (
            coreInboundGate(
                !GossipState.seenIds.contains(envelope.msgId),
                envelope.hopTtl,
                envelope.expiry,
                System.currentTimeMillis(),
            )
        ) {
            CoreInboundGate.SEEN -> {
                // Already recorded by a prior, non-concurrent copy -- no
                // record() needed, just release this claim.
                return finishAdmission(CoreInboundDisposition.SEEN, terminal = false)
            }
            CoreInboundGate.EXPIRED -> {
                Log.i(TAG, "Dropping expired envelope from $sourceLabel (expiry=${envelope.expiry})")
                // A deliberate drop is still a terminal handled state.
                return finishAdmission(CoreInboundDisposition.EXPIRED, terminal = true)
            }
            CoreInboundGate.REJECTED -> {
                Log.w(TAG, "Dropping envelope with invalid hop or expiry fields from $sourceLabel")
                return finishAdmission(CoreInboundDisposition.REJECTED, terminal = true)
            }
            CoreInboundGate.DISPATCH -> Unit
        }

        val opened = try {
            openMessage(identity, envelope.sealed)
        } catch (e: CoreException) {
            // Pairwise open failed: either foreign 1:1 traffic, or a group
            // envelope sealed with a shared key (DESIGN.md §6.5). Try groups
            // whose recipient_hint matches before treating it as pure mule
            // traffic. Group members keep relaying/carrying so absent members
            // still get a copy (mesh_sim group scenario).
            val groupOpened = tryOpenGroupMessage(envelope.recipientHint, envelope.sealed)
            if (groupOpened != null) {
                val arrival = messageArrival(sourceAddress, envelope.hopTtl, groupOpened.second.senderUserId)
                try {
                    deliverOpenedGroupEnvelope(
                        sourceLabel,
                        groupOpened.first,
                        groupOpened.second,
                        identity,
                        arrival,
                        envelope.msgId,
                    )
                } catch (e: CoreException) {
                    // T4-06: same as the pairwise path below -- a store
                    // failure delivering our own group copy must not unwind
                    // the thread, must leave the msg_id re-presentable, and
                    // must not be acked. The best-effort relay/carry for
                    // absent members is skipped; the next re-presentation
                    // re-runs the whole branch.
                    Log.w(TAG, "Deferring group envelope from $sourceLabel: durable delivery failed (${e.message})")
                    return finishAdmission(CoreInboundDisposition.FAILED, terminal = false)
                }
                // specs/group-relay-durability.md §4.3 no-reinjection rule:
                // a relay-fetched group message addressed to OUR OWN hint is
                // a per-member fan-out copy -- the relay fan-out already
                // reaches every member durably, so re-flooding/carrying it
                // would give the same content a second flood identity under
                // the fan-out msg_id. Legacy group-hint relay rows and every
                // BLE/LAN-sourced group frame keep the flood+carry behavior.
                val ownFanoutCopy = sourceAddress == null &&
                    coreIsOwnFanoutHint(envelope.recipientHint, identity.userId, System.currentTimeMillis())
                if (!ownFanoutCopy) {
                    relayForeignEnvelope(sourceAddress, envelope)
                    if (sourceAddress == null) {
                        carryRelayEnvelope(envelope)
                    } else {
                        carryForeignEnvelope(envelope, forceFamily = true)
                    }
                }
                // DTN D4: [deliverOpenedGroupEnvelope] durably stores our own
                // copy and throws (rather than returning) on a store
                // failure, so reaching this line means we already have it --
                // record regardless of whether the best-effort mule copy for
                // absent members above was stored.
                return finishAdmission(CoreInboundDisposition.CONSUMED, terminal = true)
            }
            // Not for us (or unopenable) -> foreign traffic. Two jobs, both
            // best-effort (DESIGN.md §5.3): flood it to whoever's connected
            // right now, and carry it so we can hand it to its recipient the
            // next time we meet them, even if that's hours from now.
            relayForeignEnvelope(sourceAddress, envelope)
            val carried = if (sourceAddress == null) {
                carryRelayEnvelope(envelope)
            } else {
                carryForeignEnvelope(envelope)
            }
            // DTN D4: only record once the durable carry actually succeeded.
            // [carryForeignEnvelope]/[carryRelayEnvelope] catch their own
            // store exceptions and report failure via their Boolean return
            // instead of throwing, so a disk-full failure here leaves this
            // msg_id unrecorded: the next copy of this envelope on any link
            // re-gates as Dispatch and gets another chance to carry it,
            // instead of being silently dropped as Seen for the rest of the
            // process lifetime.
            return finishAdmission(CoreInboundDisposition.CARRIED, terminal = carried)
        }
        val arrival = messageArrival(sourceAddress, envelope.hopTtl, opened.senderUserId)
        try {
            deliverOpenedEnvelope(sourceLabel, sourceAddress != null, opened, identity, arrival, envelope.msgId)
        } catch (e: CoreException) {
            // T4-06: [deliverOpenedEnvelope] does not swallow store exceptions
            // (see [handleIncomingChatMessage] etc.), so a throw here means a
            // message that was OURS to open failed to persist (disk full,
            // corrupt store). Translate it instead of letting it unwind: the
            // receive thread / relay batch loop must not be torn down, the
            // msg_id stays unrecorded so the next copy re-dispatches, and
            // FAILED is never acked so the relay copy survives for that retry.
            Log.w(TAG, "Deferring envelope from $sourceLabel: durable delivery failed (${e.message})")
            return finishAdmission(CoreInboundDisposition.FAILED, terminal = false)
        }
        // DTN D4: reaching here means the message was durably stored -- safe,
        // and required, to record.
        return finishAdmission(CoreInboundDisposition.CONSUMED, terminal = true)
    }

    private fun messageArrival(
        sourceAddress: String?,
        receivedHopTtl: UByte,
        senderUserId: ByteArray,
    ): MessageArrival {
        val linkPeerMatchesSender = sourceAddress
            ?.let(MeshRouter::userIdFor)
            ?.contentEquals(senderUserId) == true
        val linkTransport = sourceAddress?.let(MeshRouter::transportFor)
        return MessageArrival(
            transport = arrivalTransport(sourceAddress == null, linkPeerMatchesSender, linkTransport),
            hopsTaken = arrivalHopsTaken(receivedHopTtl, DEFAULT_HOP_TTL),
            receivedAt = System.currentTimeMillis(),
        )
    }

    /**
     * Opens [sealed] with any imported group whose recent-day `recipient_hint`
     * matches [recipientHint]. Returns the matching [Group] and opened
     * payload, or null. [openGroupMessage] does not check membership of the
     * signer; callers must enforce that before trusting the body.
     */
    private fun tryOpenGroupMessage(
        recipientHint: ByteArray,
        sealed: ByteArray,
    ): Pair<Group, OpenedMessage>? {
        val now = System.currentTimeMillis()
        for (group in store.listGroups()) {
            if (!recentHintsFor(group.id, now).any { it.contentEquals(recipientHint) }) continue
            try {
                return group to openGroupMessage(group, sealed)
            } catch (_: CoreException) {
                // Wrong key / corrupt — try the next matching group (rare).
            }
        }
        return null
    }

    /**
     * Adds a foreign envelope to the persistent carry queue (DESIGN.md §5.3
     * store-and-forward). Classifies it as "family" -- addressed to someone we
     * know -- when its `recipient_hint` matches a contact ([hintMatchesAnyContact]);
     * family envelopes win eviction fights, while foreign ones share a bounded
     * [FOREIGN_CARRY_BUDGET_BYTES] budget and the core bounds the whole queue.
     * Idempotent on `msg_id`, so re-seeing an envelope we already carry is a
     * no-op. Reached only after [handleEnvelope]'s dedupe + expiry gates, so
     * we never carry a stale duplicate or an already-expired envelope.
     *
     * The stored `hop_ttl` is [carriedHopTtl] of the received value, not the
     * value verbatim: this device's carry of the envelope is itself a hop, so
     * it must be counted like the flood path counts its own re-relays (see
     * [relayForeignEnvelope]) -- otherwise [arrivalHopsTaken] under-counts a
     * pure mule delivery by one. See [carriedHopTtl]'s KDoc for the full
     * rationale and the zero-TTL saturation guarantee.
     *
     * Returns `true` if the store operation completed (whether it newly
     * queued the envelope or found it already carried) and `false` if the
     * store call itself failed. DTN D4: [processInboundEnvelope] uses this
     * return value to decide whether it's safe to mark the envelope's
     * `msg_id` seen -- see its KDoc.
     */
    private fun carryForeignEnvelope(envelope: Frame.Envelope, forceFamily: Boolean = false): Boolean {
        val now = System.currentTimeMillis()
        return try {
            val isFamily = forceFamily || hintMatchesKnownTarget(envelope.recipientHint, now)
            val stored = store.enqueueCarriedEnvelope(
                CarriedEnvelope(
                    msgId = envelope.msgId,
                    hopTtl = carriedHopTtl(envelope.hopTtl),
                    expiry = envelope.expiry,
                    recipientHint = envelope.recipientHint,
                    sealed = envelope.sealed,
                ),
                isFamily,
                now,
                FOREIGN_CARRY_BUDGET_BYTES,
            )
            if (stored) {
                Log.i(TAG, "Carrying foreign envelope (family=$isFamily) for later delivery")
                if (isFamily) {
                    requestRelaySync("family carry queued")
                }
            }
            true
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to enqueue carried envelope: ${e.message}")
            false
        }
    }

    /**
     * Relay-sourced twin of [carryForeignEnvelope]: adds an envelope we
     * fetched FROM the relay (relay proxy-polling, [relayProxyHints]) to the
     * persistent carry queue for BLE delivery to its real recipient. Unlike
     * [carryForeignEnvelope], this deliberately does NOT call
     * [requestRelaySync] -- the envelope is already sitting on the relay (that
     * is where we just fetched it from), so re-uploading it would only churn
     * traffic and risk resurrecting a copy the real recipient already acked.
     * [MessageStore.enqueueRelayCarriedEnvelope] enforces this on the core
     * side too (`from_relay = 1` is excluded from the upload query), so this
     * is belt-and-suspenders, but skipping the call here avoids scheduling a
     * pointless relay-sync pass. Idempotent on `msg_id` like its sibling.
     *
     * Also mirrors [carryForeignEnvelope] in storing [carriedHopTtl] of the
     * received `hop_ttl` rather than the raw value -- this device is muling
     * the envelope the same as the BLE-sourced case, so the same hop must be
     * counted.
     *
     * Returns `true`/`false` on store success/failure -- see
     * [carryForeignEnvelope]'s KDoc for why [processInboundEnvelope] needs
     * this (DTN D4).
     */
    private fun carryRelayEnvelope(envelope: Frame.Envelope): Boolean {
        val now = System.currentTimeMillis()
        return try {
            val stored = store.enqueueRelayCarriedEnvelope(
                CarriedEnvelope(
                    msgId = envelope.msgId,
                    hopTtl = carriedHopTtl(envelope.hopTtl),
                    expiry = envelope.expiry,
                    recipientHint = envelope.recipientHint,
                    sealed = envelope.sealed,
                ),
                now,
            )
            if (stored) {
                Log.i(TAG, "Carrying relay-sourced envelope (proxy) for later BLE delivery")
            }
            true
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to enqueue relay-carried envelope: ${e.message}")
            false
        }
    }

    /**
     * Hands over every carried envelope destined for the peer that just
     * HELLO'd on [address] (DESIGN.md §5.3): we compute the peer's recent-day
     * `recipient_hint`s ([recentHintsFor]) and pull matching envelopes from
     * the store, and send each on this link. Expired entries are pruned
     * first. If the peer already saw an envelope via an earlier flood, their
     * own seen-ID set drops the duplicate harmlessly; if they didn't (the
     * whole point -- they were out of range when it flooded), this is how it
     * reaches them.
     *
     * `env.hopTtl` here is forwarded verbatim -- it's already [carriedHopTtl]
     * of what this device originally received, decremented once at
     * [carryForeignEnvelope]/[carryRelayEnvelope] enqueue time, not the raw
     * value the frame arrived with. No further decrement happens here.
     *
     * DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): this function only ever
     * *attempts* delivery -- it no longer calls [MessageStore.removeCarriedEnvelope]
     * on a successful [MeshRouter.sendToAddress]. That return only means a
     * transport function accepted the write (e.g. [BleCentral]'s `sendFrame`
     * just enqueues fragments into a per-address write queue), not that the
     * bytes made it to the peer; a disconnect mid-transfer used to silently
     * drop the whole write queue after we'd already deleted our only copy.
     * The carried row is now removed later, once the peer's own next digest
     * exchange proves they actually have it -- see
     * [MessageStore.coreConfirmCarriedDeliveries], called from
     * [sprayDigestPlanTo].
     *
     * Invariant, stated verbatim (DTN_TODOS.md §3.2): worst case of a
     * dropped mid-transfer link is a harmless duplicate resend (the peer's
     * seen-set/store dedupes it), never a lost envelope; an unconfirmed
     * carry still dies at its normal expiry via [MessageStore.pruneExpiredCarried].
     */
    private fun drainCarriedEnvelopesTo(address: String, peerUserId: ByteArray) {
        val now = System.currentTimeMillis()
        try {
            store.pruneExpiredCarried(now)
            // Peer userId hints plus every group that peer is a member of
            // (DESIGN.md §6.5: members mule for the whole group).
            val hints = deliveryHintsForPeer(peerUserId, now)
            val toDeliver = store.carriedEnvelopesForHints(hints, now)
            if (toDeliver.isEmpty()) return
            var delivered = 0
            for (env in toDeliver) {
                val frame = encodeEnvelopeFrame(env.msgId, env.hopTtl, env.expiry, env.recipientHint, env.sealed)
                if (MeshRouter.sendToAddress(address, frame)) {
                    delivered++
                }
            }
            Log.i(TAG, "Attempted delivery of $delivered carried envelope(s) to $address (removal awaits their digest confirmation)")
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to drain carried envelopes to $address: ${e.message}")
        }
    }

    /**
     * `recipient_hint`s the peer can open: their own userId over recent days,
     * plus every imported group they belong to.
     */
    private fun deliveryHintsForPeer(peerUserId: ByteArray, now: Long): List<ByteArray> {
        val hints = recentHintsFor(peerUserId, now).toMutableList()
        for (group in store.listGroups()) {
            if (group.memberUserIds.any { it.contentEquals(peerUserId) }) {
                hints += recentHintsFor(group.id, now)
            }
        }
        return hints
    }

    /** True if [hint] matches a known contact or imported group (family traffic). */
    private fun hintMatchesKnownTarget(hint: ByteArray, now: Long): Boolean {
        for (contact in store.listContacts()) {
            if (recentHintsFor(contact.userId, now).any { it.contentEquals(hint) }) {
                return true
            }
        }
        return recognizesGroupHint(hint, now)
    }

    private fun recognizesGroupHint(hint: ByteArray, now: Long): Boolean =
        groupMatchingHint(hint, now) != null

    /**
     * The imported group whose recent-day hints include [hint], if any --
     * the group-shaped sibling of [contactMatchingHint], used by the fan-out
     * upload path (specs/group-relay-durability.md §4.2) which needs the
     * group's member list, not just a yes/no.
     */
    private fun groupMatchingHint(hint: ByteArray, now: Long): Group? {
        for (group in store.listGroups()) {
            if (recentHintsFor(group.id, now).any { it.contentEquals(hint) }) {
                return group
            }
        }
        return null
    }

    /**
     * The `recipient_hint`s [userId] could match for a still-carriable
     * envelope: their UserID hashed against each day-number from today back
     * through [CARRY_HINT_DAY_WINDOW] days (DESIGN.md §6.4's daily-rotating
     * hint over the §5.3 expiry window).
     */
    private fun recentHintsFor(userId: ByteArray, now: Long): List<ByteArray> =
        (0..CARRY_HINT_DAY_WINDOW).map { daysAgo ->
            computeRecipientHint(userId, now - daysAgo * MS_PER_DAY)
        }

    private fun recentPresenceHintsFor(userId: ByteArray, now: Long): List<ByteArray> =
        (0..PRESENCE_HINT_DAY_WINDOW).map { daysAgo ->
            computeRecipientHint(userId, now - daysAgo * MS_PER_DAY)
        }

    /**
     * Floods a foreign (not-for-us) envelope onward per DESIGN.md §5.3, if it
     * still has hop budget. `hop_ttl` is the remaining number of hops; we
     * decrement it and forward only while at least one hop would remain
     * (`hop_ttl > 1`), so a frame arriving with `hop_ttl == 1` is the last
     * carrier's copy and stops here. The `msg_id`, `expiry`, `recipient_hint`,
     * and sealed bytes are all preserved verbatim -- only `hop_ttl` changes --
     * so every carrier along the way computes the same dedupe key. The
     * arriving link is excluded from the flood to avoid the trivial echo;
     * the mesh's other seen-ID sets stop longer loops once the recipients
     * record this `msg_id` themselves.
     *
     * DTN D4 / FA5 loop-hazard note: since [processInboundEnvelope] moved to
     * check-then-record, [GossipState.seenIds] is *not yet* updated for this
     * `msg_id` at the moment this call happens (it's recorded, via
     * [InboundEnvelopeAdmission.finish], after this function returns, once
     * the whole terminal branch succeeds -- see [processInboundEnvelope]'s
     * KDoc). This is still safe against self-re-ingestion, but *not* for the
     * reason an earlier version of this note claimed:
     * [processInboundEnvelope] does **not** run synchronously per received
     * frame -- it is called concurrently from up to four receive-path
     * threads (central-GATT binder, peripheral-GATT binder, LanTransport's
     * `connectionExecutor`, and the relay-sync thread), and two copies of one
     * `msg_id` arriving on different transports at once is routine for a
     * nearby contact. What actually rules out same-node re-entrancy for
     * *this* `msg_id` before the terminal record lands is
     * [InboundEnvelopeAdmission]'s atomic in-flight claim: a concurrent
     * second copy of this exact `msg_id`, on any thread, is rejected at the
     * top of [processInboundEnvelope] before it ever reaches this function.
     * Combined with the arriving link being excluded from the fanout above
     * (so this node can't hand the relayed frame straight back to itself),
     * a frame this node relays could only loop back from a third node's
     * rebroadcast, which takes at least one more hop and one more link
     * round-trip -- by then this node's record has long since happened.
     */
    private fun relayForeignEnvelope(address: String?, envelope: Frame.Envelope) {
        val remainingHops = envelope.hopTtl.toInt()
        if (remainingHops <= 1) {
            // Hop budget exhausted; this node is the final carrier for it.
            return
        }
        val relayed = encodeEnvelopeFrame(
            envelope.msgId,
            (remainingHops - 1).toUByte(),
            envelope.expiry,
            envelope.recipientHint,
            envelope.sealed,
        )
        val fanout = if (address == null) {
            MeshRouter.relayToAll(relayed)
        } else {
            MeshRouter.relayToAllExcept(address, relayed)
        }
        if (fanout > 0) {
            Log.i(
                TAG,
                "Relayed foreign envelope from ${address ?: "relay"} to $fanout link(s), " +
                    "hop_ttl ${remainingHops}->${remainingHops - 1}",
            )
        }
    }

    /**
     * Delivers an envelope we successfully opened (DESIGN.md §6.3 open/verify,
     * §7.1 body layout). See this class's KDoc for why
     * `body.chatId == opened.senderUserId` is the correct sanity check here.
     * Reached only for envelopes addressed to us; foreign traffic never gets
     * here (see [handleEnvelope]).
     */
    private fun deliverOpenedEnvelope(
        address: String,
        directBle: Boolean,
        opened: OpenedMessage,
        identity: Identity,
        arrival: MessageArrival,
        msgId: ByteArray,
    ) {
        val extendedBody = try {
            decodeExtendedMessageBody(opened.payload)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping envelope from $address: failed to decode body (${e.message})")
            return
        }
        val body = MessageBody(
            kind = extendedBody.kind,
            chatId = extendedBody.chatId,
            lamport = extendedBody.lamport,
            timestamp = extendedBody.timestamp,
            content = extendedBody.content,
        )
        if (!body.chatId.contentEquals(opened.senderUserId)) {
            Log.w(TAG, "Dropping envelope from $address: chatId does not match the verified sender")
            return
        }
        val senderIsContact = store.getContact(opened.senderUserId) != null
        if (
            !corePairwiseSenderAuthorized(
                body.kind,
                senderIsContact,
                opened.senderUserId.contentEquals(identity.userId),
            )
        ) {
            Log.w(TAG, "Dropping envelope from $address: sender is not authorized for kind=${body.kind}")
            return
        }

        when (body.kind) {
            KIND_TEXT -> handleIncomingChatMessage(
                address,
                opened.senderUserId,
                body,
                identity,
                KIND_TEXT,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            KIND_ATTACHMENT_MANIFEST -> handleIncomingChatMessage(
                address,
                opened.senderUserId,
                body,
                identity,
                KIND_ATTACHMENT_MANIFEST,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            KIND_REACTION -> handleIncomingChatMessage(
                address,
                opened.senderUserId,
                body,
                identity,
                KIND_REACTION,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            KIND_RECEIPT -> handleIncomingReceipt(
                address,
                opened.senderUserId,
                body,
                identity,
                arrival,
            )
            KIND_FRIEND_REQUEST -> handleIncomingFriendRequest(address, directBle, opened.senderUserId, body, identity)
            KIND_GROUP_INVITE -> handleIncomingGroupInvite(address, opened.senderUserId, body, identity)
            KIND_PROFILE_SYNC -> handleIncomingProfileSync(address, opened.senderUserId, body, identity)
            KIND_FRIEND_DIRECTORY -> handleIncomingFriendDirectory(address, opened.senderUserId, body, identity)
            KIND_INTRODUCED_FRIEND_REQUEST -> handleIncomingIntroducedFriendRequest(
                address,
                directBle,
                opened.senderUserId,
                body,
                identity,
            )
            KIND_LAN_ENDPOINT_HINT -> handleIncomingLanEndpointHint(
                address,
                opened.senderUserId,
                body,
                identity,
            )
            else -> Log.i(TAG, "Dropping envelope from $address: unhandled kind=${body.kind}")
        }
    }

    private fun handleIncomingLanEndpointHint(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val contact = store.getContact(senderUserId) ?: return
        val content = try {
            decodeLanEndpointContent(body.content)
        } catch (error: CoreException) {
            Log.w(TAG, "Dropping sealed LAN endpoint hint: ${error.message}")
            return
        }
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_LAN_ENDPOINT_HINT,
                payload = body.content,
            ),
        )
        if (!inserted) return

        val hintedNetworkId = content.networkId.toString(Charsets.UTF_8)
        val endpoint = LanManualEndpoint(content.host, content.port.toInt())
        lanEndpointCache.save(hintedNetworkId, senderUserId, endpoint)
        LanCapabilityStore.markSupported(this, senderUserId)
        val now = System.currentTimeMillis()
        if (
            content.expiresAtMs > now &&
            hintedNetworkId == lanTransport?.currentNetworkId()
        ) {
            lanTransport?.connectToHint(
                Frame.LanEndpoint(
                    instanceToken = content.instanceToken,
                    host = content.host,
                    port = content.port,
                ),
                senderUserId,
            )
        }
        acknowledgeHiddenMessage(address, senderUserId, identity, contact)
    }

    /**
     * Delivers a group-sealed envelope we opened with an imported group key
     * (DESIGN.md §6.5). Wire [MessageBody.chatId] is the group id; the
     * verified signer must be a current member (core does not check this).
     * Group receipts are deferred — we only store + notify.
     */
    private fun deliverOpenedGroupEnvelope(
        address: String,
        group: Group,
        opened: OpenedMessage,
        identity: Identity,
        arrival: MessageArrival,
        msgId: ByteArray,
    ) {
        if (!group.memberUserIds.any { it.contentEquals(opened.senderUserId) }) {
            Log.w(
                TAG,
                "Dropping group envelope from $address: signer ${UserIdHex.encode(opened.senderUserId)} " +
                    "is not a member of group ${group.name}",
            )
            return
        }
        if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) {
            Log.w(TAG, "Dropping group envelope from $address: we are not a member of ${group.name}")
            return
        }

        val extendedBody = try {
            decodeExtendedMessageBody(opened.payload)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping group envelope from $address: failed to decode body (${e.message})")
            return
        }
        val body = MessageBody(
            kind = extendedBody.kind,
            chatId = extendedBody.chatId,
            lamport = extendedBody.lamport,
            timestamp = extendedBody.timestamp,
            content = extendedBody.content,
        )
        if (!body.chatId.contentEquals(group.id)) {
            Log.w(TAG, "Dropping group envelope from $address: body.chatId does not match group id")
            return
        }
        when (body.kind) {
            KIND_TEXT, KIND_ATTACHMENT_MANIFEST, KIND_REACTION -> handleIncomingGroupChatMessage(
                address,
                group,
                opened.senderUserId,
                body,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            KIND_GROUP_METADATA_UPDATE -> handleIncomingGroupMetadataUpdate(
                address,
                group,
                opened.senderUserId,
                body,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            else -> Log.i(TAG, "Dropping group envelope from $address: unhandled kind=${body.kind}")
        }
    }

    private fun handleIncomingGroupMetadataUpdate(
        address: String,
        group: Group,
        senderUserId: ByteArray,
        body: MessageBody,
        arrival: MessageArrival,
        msgId: ByteArray,
        replyToMsgId: ByteArray?,
    ) {
        val updated = try {
            val update = decodeGroupMetadataUpdate(body.content)
            applyGroupMetadataUpdate(group, update, senderUserId)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping invalid group metadata from $address: ${e.message}")
            return
        }
        val inserted = store.insertIncomingMessage(
            StoredMessage(
                chatId = group.id,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = body.kind,
                payload = body.content,
            ),
            msgId,
            replyToMsgId,
        )
        if (!inserted) return
        store.recordMessageArrival(group.id, senderUserId, body.lamport, arrival)
        if (updated != null) {
            store.upsertGroup(updated)
            Log.i(TAG, "Applied group metadata revision ${updated.metadataRevision} for ${updated.name}")
            ChatEvents.notifyChatChanged(group.id)
        }
    }

    private fun handleIncomingGroupChatMessage(
        address: String,
        group: Group,
        senderUserId: ByteArray,
        body: MessageBody,
        arrival: MessageArrival,
        msgId: ByteArray,
        replyToMsgId: ByteArray?,
    ) {
        val inserted = store.insertIncomingMessage(
            StoredMessage(
                chatId = group.id,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = body.kind,
                payload = body.content,
            ),
            msgId,
            replyToMsgId,
        )
        if (!inserted) {
            Log.i(
                TAG,
                "Ignoring duplicate group kind=${body.kind} from $address " +
                    "sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
            )
            return
        }
        store.recordMessageArrival(group.id, senderUserId, body.lamport, arrival)
        Log.i(
            TAG,
            "Stored group kind=${body.kind} in ${group.name} from $address " +
                "sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
        )
        ChatEvents.notifyChatChanged(group.id)

        // Local read watermark only (group wire receipts are deferred). Uses
        // highestLamport (plain MAX), not highestContiguousLamport: the
        // latter stalls at 0 once the sender's stream legitimately starts
        // above lamport 1 (post chat-history-wipe ratchet), which would
        // leave this watermark -- and the unread badge -- stuck forever.
        val throughLamport = store.highestLamport(group.id, senderUserId)
        store.recordOutgoingReceipt(group.id, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        val isVisible = ChatVisibility.isVisible(group.id)
        if (isVisible) {
            store.recordOutgoingReceipt(group.id, senderUserId, RECEIPT_TYPE_READ, throughLamport)
        } else if (isVisibleChatKind(body.kind)) {
            val senderName = store.getContact(senderUserId)?.let(::coreContactDisplayName)
                ?: UserIdHex.encode(senderUserId).take(8)
            val preview = if (body.kind == KIND_ATTACHMENT_MANIFEST) {
                try {
                    AttachmentPayload.previewLabel(AttachmentPayload.decode(body.content))
                } catch (_: Exception) {
                    "Attachment"
                }
            } else {
                body.content.toString(Charsets.UTF_8)
            }
            MessageNotifier.notifyIncomingGroupMessage(this, group, senderName, preview)
        }
    }

    /**
     * Imports a pairwise-sealed `kind=4` group invite (DESIGN.md §6.5). Wire
     * `chatId` is the invite sender's userId (1:1 pairwise convention); the
     * group id/key/members live in the invite content. Local history is stored
     * under `chat_id = group.id`.
     */
    private fun handleIncomingGroupInvite(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val group = try {
            decodeGroupInviteContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping group invite from $address: failed to decode (${e.message})")
            return
        }
        if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) {
            Log.w(TAG, "Dropping group invite from $address: we are not listed as a member")
            return
        }
        if (!group.memberUserIds.any { it.contentEquals(senderUserId) }) {
            Log.w(TAG, "Dropping group invite from $address: sender is not listed as a member")
            return
        }

        store.upsertGroup(group)
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = group.id,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_GROUP_INVITE,
                payload = body.content,
            ),
        )
        if (!inserted) {
            Log.i(TAG, "Ignoring duplicate group invite for ${group.name} from $address")
            return
        }
        ChatEvents.notifyChatChanged(group.id)
        Log.i(TAG, "Imported group ${group.name} from invite on $address")

        val contact = store.getContact(senderUserId)
        if (contact != null) {
            // 1:1 delivered/read receipts still apply to the pairwise invite
            // envelope's sender stream — but the invite row lives under the
            // group chat, so we only ack if we also store something under the
            // 1:1 chat. Skip wire receipts for invites; the group is what matters.
        }
        if (!ChatVisibility.isVisible(group.id)) {
            // FA8: a typed entry point, not a literal string sniffed by
            // MessageNotifier's prefix check -- see notifyGroupInvite's KDoc.
            MessageNotifier.notifyGroupInvite(this, group)
        }
    }

    /**
     * Stores a signed `kind=3` friend request in the hidden lamport stream and
     * imports/updates the sender as a contact from the authenticated payload.
     * The payload is a FriendCard JSON string, but unlike a QR scan we can
     * verify it matches the envelope sender's signing key before trusting it.
     */
    private fun handleIncomingFriendRequest(
        address: String,
        directBle: Boolean,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val pendingSuggestion = store.listFriendSuggestions(System.currentTimeMillis()).firstOrNull {
            it.state == 1.toUByte() && it.candidate.userId.contentEquals(senderUserId)
        }
        val card = try {
            parseFriendCard(body.content.toString(Charsets.UTF_8))
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping friend request from $address: failed to parse FriendCard (${e.message})")
            return
        }
        if (!friendCardUserId(card).contentEquals(senderUserId)) {
            Log.w(TAG, "Dropping friend request from $address: payload identity doesn't match verified sender")
            return
        }

        val wasKnown = store.getContact(senderUserId) != null
        val contact = RelayImport.reconcileOnImport(
            this,
            store,
            Contact(
                userId = senderUserId,
                name = card.name,
                signPk = card.signPk,
                agreePk = card.agreePk,
                relayUrl = card.relayUrl,
                relayToken = card.relayToken,
            ),
        )
        store.upsertContactProvenance(
            ContactProvenance(
                userId = senderUserId,
                source = if (pendingSuggestion == null) 0u else 1u,
                introducerUserId = pendingSuggestion?.introducerUserId,
                introducedAtMs = System.currentTimeMillis(),
            ),
        )
        if (pendingSuggestion != null) store.removeFriendSuggestion(senderUserId)
        ProfileSyncSender.queueToContact(
            this,
            store,
            identity,
            contact,
            ProfileStore.loadOwnAvatarEpoch(this),
        )
        sendLanEndpointHintTo(address)
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_FRIEND_REQUEST,
                payload = body.content,
            ),
        )
        if (!inserted) return
        ChatEvents.notifyChatChanged(senderUserId)

        // highestLamport (plain MAX), not highestContiguousLamport: this is
        // a watermark over the peer's stream, and after the lamport ratchet
        // that stream can legitimately start above 1, where the contiguous
        // count would stall at 0 forever.
        val throughLamport = store.highestLamport(senderUserId, senderUserId)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        var relayQueueChanged = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        val isVisible = ChatVisibility.isVisible(senderUserId)
        if (isVisible) {
            store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_READ, throughLamport)
            relayQueueChanged = queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = senderUserId,
                throughLamport = throughLamport,
            ) || relayQueueChanged
        }
        if (relayQueueChanged) {
            RelaySyncEvents.requestSync()
        }

        sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_DELIVERED, senderUserId, throughLamport)
        if (isVisible) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, senderUserId, throughLamport)
        }
        if (!wasKnown) {
            FriendImportEvents.notifyImported(contact, directBle)
            MessageNotifier.notifyFriendAdded(this, contact)
        }
        Log.i(TAG, "Imported contact ${contact.name} from friend request on $address")
    }

    private fun handleIncomingProfileSync(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val existing = store.getContact(senderUserId)
        if (existing == null) {
            Log.i(TAG, "Dropping profile sync from $address: sender is not a contact")
            return
        }
        val content = try {
            decodeProfileSyncContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping profile sync from $address: failed to decode (${e.message})")
            return
        }
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_PROFILE_SYNC,
                payload = body.content,
            ),
        )
        if (!inserted) return

        val policyChanged = store.upsertContactDiscoveryPolicy(
            ContactDiscoveryPolicy(
                userId = senderUserId,
                protocolVersion = content.friendsOfFriendsVersion,
                enabled = content.friendsOfFriendsEnabled,
                revision = content.friendsOfFriendsRevision,
            ),
        )

        val applied = store.setContactAvatar(
            senderUserId,
            content.avatar.takeIf { it.isNotEmpty() },
            content.avatarEpoch,
        )
        if (applied && content.name != existing.name) {
            store.upsertContact(existing.copy(name = content.name))
        }
        ChatEvents.notifyChatChanged(senderUserId)

        val contact = store.getContact(senderUserId) ?: existing
        val throughLamport = store.highestLamport(senderUserId, senderUserId)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        var relayQueueChanged = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        val isVisible = ChatVisibility.isVisible(senderUserId)
        if (isVisible) {
            store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_READ, throughLamport)
            relayQueueChanged = queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = senderUserId,
                throughLamport = throughLamport,
            ) || relayQueueChanged
        }
        if (relayQueueChanged) {
            RelaySyncEvents.requestSync()
        }

        sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_DELIVERED, senderUserId, throughLamport)
        if (isVisible) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, senderUserId, throughLamport)
        }
        if (policyChanged) {
            FriendDirectorySender.queueToAllContacts(this, store, identity)
        }
    }

    private fun handleIncomingFriendDirectory(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val contact = store.getContact(senderUserId) ?: run {
            Log.i(TAG, "Dropping friend directory from $address: sender is not a contact")
            return
        }
        val content = try {
            decodeFriendDirectoryContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping friend directory from $address: ${e.message}")
            return
        }
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_FRIEND_DIRECTORY,
                payload = body.content,
            ),
        )
        if (!inserted) return
        if (FriendsOfFriendsStore.isEnabled(this)) {
            try {
                if (store.applyFriendDirectory(senderUserId, identity.userId, content, System.currentTimeMillis())) {
                    ChatEvents.notifyChatChanged(senderUserId)
                }
            } catch (e: CoreException) {
                Log.w(TAG, "Rejecting friend directory from $address: ${e.message}")
                return
            }
        }
        acknowledgeHiddenMessage(address, senderUserId, identity, contact)
    }

    private fun handleIncomingIntroducedFriendRequest(
        address: String,
        directBle: Boolean,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        if (!FriendsOfFriendsStore.isEnabled(this)) {
            Log.i(TAG, "Ignoring introduced friend request while friends-of-friends is disabled")
            return
        }
        val request = try {
            decodeIntroducedFriendRequest(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping introduced friend request from $address: ${e.message}")
            return
        }
        val card = try {
            parseFriendCard(request.friendCardJson)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping introduced friend request with invalid card: ${e.message}")
            return
        }
        if (!friendCardUserId(card).contentEquals(senderUserId)) {
            Log.w(TAG, "Dropping introduced friend request: card does not match authenticated sender")
            return
        }
        val introducer = store.getContact(request.ticket.introducerUserId) ?: run {
            Log.w(TAG, "Dropping introduced friend request: introducer is no longer a contact")
            return
        }
        val valid = try {
            verifyIntroductionTicket(
                request.ticket,
                introducer.signPk,
                identity.userId,
                senderUserId,
                FriendsOfFriendsStore.revision(this),
                System.currentTimeMillis(),
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping introduced friend request: ${e.message}")
            return
        }
        if (!valid) {
            Log.w(TAG, "Dropping introduced friend request: ticket validation failed")
            return
        }

        val wasKnown = store.getContact(senderUserId) != null
        val contact = RelayImport.reconcileOnImport(
            this,
            store,
            Contact(
                userId = senderUserId,
                name = card.name,
                signPk = card.signPk,
                agreePk = card.agreePk,
                relayUrl = card.relayUrl,
                relayToken = card.relayToken,
            ),
        )
        store.upsertContactProvenance(
            ContactProvenance(
                userId = senderUserId,
                source = 1u,
                introducerUserId = introducer.userId,
                introducedAtMs = System.currentTimeMillis(),
            ),
        )
        store.removeFriendSuggestion(senderUserId)
        store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_INTRODUCED_FRIEND_REQUEST,
                payload = body.content,
            ),
        )
        acknowledgeHiddenMessage(address, senderUserId, identity, contact)
        FriendRequestSender.queueForScannedContact(this, store, identity, contact)
        ProfileSyncSender.queueToContact(
            this,
            store,
            identity,
            contact,
            ProfileStore.loadOwnAvatarEpoch(this),
        )
        sendLanEndpointHintTo(address)
        if (!wasKnown) FriendDirectorySender.queueToAllContacts(this, store, identity)
        ChatEvents.notifyChatChanged(senderUserId)
        if (!wasKnown) {
            FriendImportEvents.notifyImported(contact, directBle)
            MessageNotifier.notifyFriendAdded(this, contact)
        }
    }

    private fun acknowledgeHiddenMessage(
        address: String,
        senderUserId: ByteArray,
        identity: Identity,
        contact: Contact,
    ) {
        val throughLamport = store.highestLamport(senderUserId, senderUserId)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        val queued = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        if (queued) RelaySyncEvents.requestSync()
        sendReceiptOnAddress(
            identity,
            contact,
            address,
            RECEIPT_TYPE_DELIVERED,
            senderUserId,
            throughLamport,
        )
    }

    /**
     * Stores an incoming text message and, only if it was newly inserted,
     * sends a delivered receipt back on the same link (DESIGN.md §7.2), plus
     * -- if the chat is currently on screen ([ChatVisibility.isVisible]) -- a
     * read receipt too. Otherwise, posts a notification
     * ([MessageNotifier.notifyIncomingMessage]) instead, since the chat isn't
     * visible for the user to see the message land. Those two are mutually
     * exclusive by construction (`if (visible) read-receipt else notify`),
     * which matches the product intent: no point notifying about a chat the
     * user is already looking at, and no point sending a read receipt for
     * one they aren't.
     *
     * A duplicate insert (e.g. re-sent by the peer's digest sync above,
     * DESIGN.md §7.3) is a silent no-op here -- it was already acknowledged
     * (and, if applicable, notified) the first time, and redoing either
     * wouldn't change anything, so this path can never send two receipts or
     * two notifications for one message.
     *
     * This never triggers another receipt (see [handleIncomingReceipt]):
     * receipts are kind=2, this branch only ever runs for chat-stream kinds
     * (text / attachment-manifest), and [handleIncomingReceipt] never calls
     * [sendReceiptOnAddress] or [sendReceiptToContact] or otherwise sends
     * anything back. Combined with authored resend only ever replaying kinds
     * that *we* originated (text, attachment, friend-request — never a
     * receipt), there's no cycle where a receipt causes a receipt.
     */
    private fun handleIncomingChatMessage(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
        kind: UByte,
        arrival: MessageArrival,
        msgId: ByteArray,
        replyToMsgId: ByteArray?,
    ) {
        val inserted = store.insertIncomingMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = kind,
                payload = body.content,
            ),
            msgId,
            replyToMsgId,
        )
        if (!inserted) {
            Log.i(
                TAG,
                "Ignoring duplicate kind=$kind from $address sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
            )
            return
        }
        store.recordMessageArrival(senderUserId, senderUserId, body.lamport, arrival)
        Log.i(
            TAG,
            "Stored kind=$kind from $address sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
        )
        MeshConnectivityStatus.mergeLastSeen(UserIdHex.encode(senderUserId), System.currentTimeMillis())
        ChatEvents.notifyChatChanged(senderUserId)

        // highestLamport (plain MAX), not highestContiguousLamport: same
        // peer-stream-watermark reasoning as above -- a contiguous-from-1
        // count stalls at 0 once the sender's stream starts above 1 (post
        // ratchet), stranding the delivered/read receipt and unread badge.
        val throughLamport = store.highestLamport(senderUserId, senderUserId)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        var relayQueueChanged = false
        val isVisible = ChatVisibility.isVisible(senderUserId)
        if (isVisible) {
            store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_READ, throughLamport)
        }

        val contact = store.getContact(senderUserId)
        if (contact == null) {
            // We stored the message (friending can happen independently of
            // messaging order), but with no contact we have no agreePk to
            // seal a receipt to, and nothing sensible to show in a
            // notification (no display name, no key to trust it came from
            // who it claims), so skip both.
            Log.i(TAG, "Stored a message from unrecognized userId=${UserIdHex.encode(senderUserId)}; no receipt/notification")
            return
        }

        relayQueueChanged = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        if (isVisible) {
            relayQueueChanged = queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = senderUserId,
                throughLamport = throughLamport,
            ) || relayQueueChanged
        }
        if (relayQueueChanged) {
            RelaySyncEvents.requestSync()
        }

        sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_DELIVERED, senderUserId, throughLamport)

        if (isVisible) {
            // The user is already looking at this chat, so it was read the
            // instant it landed -- send the read receipt now rather than
            // waiting for ChatViewEvents, which only fires when a chat
            // *becomes* visible, not for messages arriving while it already is.
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, senderUserId, throughLamport)
        } else if (isVisibleChatKind(kind)) {
            val preview = when (kind) {
                KIND_ATTACHMENT_MANIFEST ->
                    AttachmentPayload.previewLabel(AttachmentPayload.decode(body.content))
                else -> body.content.toString(Charsets.UTF_8)
            }
            MessageNotifier.notifyIncomingMessage(this, contact, preview)
        }
    }

    /**
     * Persists an incoming receipt as a delivered/read watermark on our own
     * outgoing messages (DESIGN.md §7.2) and pings [ChatEvents] so any open
     * chat screen redraws its ✓/✓✓ ticks.
     *
     * Two sanity checks before trusting it, both log-and-drop on failure:
     * - `receipt.senderUserId` must be OUR OWN userId. A receipt only ever
     *   acknowledges messages *we* authored in a 1:1 chat -- a peer has no
     *   business acking someone else's messages to us, so anything else here
     *   is either a bug or a malicious/confused peer.
     * - The outer envelope's verified sender ([envelopeSenderUserId], from
     *   [handleEnvelope]'s `openMessage`) must be a known contact, since
     *   that's the local `chatId` this receipt gets recorded under (see
     *   below) and we only track receipts for chats we actually have.
     *
     * `store.recordReceipt`'s `senderUserId` param is OUR OWN userId here --
     * not [envelopeSenderUserId] -- because it names whose *messages* the
     * receipt is about (ours), while `chatId` is [envelopeSenderUserId]
     * because locally a 1:1 chat is keyed by the other party (see class
     * KDoc). This never sends anything back, so it cannot loop into another
     * receipt (see [handleIncomingText]'s KDoc for the full argument).
     */
    private fun handleIncomingReceipt(
        address: String,
        envelopeSenderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
        arrival: MessageArrival,
    ) {
        val receipt = try {
            decodeReceiptContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping receipt from $address: failed to decode (${e.message})")
            return
        }
        if (!receipt.senderUserId.contentEquals(identity.userId)) {
            Log.w(
                TAG,
                "Dropping receipt from $address: acks senderUserId=${UserIdHex.encode(receipt.senderUserId)}, " +
                    "not us -- peers can only ack messages we authored",
            )
            return
        }
        val contact = store.getContact(envelopeSenderUserId)
        if (contact == null) {
            Log.w(TAG, "Dropping receipt from $address: envelope sender ${UserIdHex.encode(envelopeSenderUserId)} is not a known contact")
            return
        }

        Log.i(
            TAG,
            "Receipt from $address: ackedSender=${UserIdHex.encode(receipt.senderUserId)} " +
                "throughLamport=${receipt.lamport} type=${receipt.receiptType}",
        )
        MeshConnectivityStatus.mergeLastSeen(UserIdHex.encode(envelopeSenderUserId), System.currentTimeMillis())
        // The receipt returned on the exact link that delivered the message;
        // record that route against the watermark (T6) so every acknowledged
        // message's Info pane can prove LAN/BLE/relay delivery -- not just the
        // one at the exact watermark lamport.
        store.recordReceipt(
            chatId = envelopeSenderUserId, // local convention: chat keyed by the other party -- see class KDoc
            senderUserId = identity.userId, // whose messages this receipt is about: ours
            receiptType = receipt.receiptType,
            throughLamport = receipt.lamport,
            viaTransport = arrival.transport,
        )
        // V2 field metric: stamp delivery latency + route on the messages this
        // (cumulative) delivery receipt confirms. READ receipts imply delivery
        // too, but the DELIVERED watermark is the one we measure against.
        if (receipt.receiptType == RECEIPT_TYPE_DELIVERED) {
            runCatching {
                store.recordDeliveredMetric(
                    chatId = envelopeSenderUserId,
                    throughLamport = receipt.lamport,
                    deliveredAtMs = arrival.receivedAt,
                    viaTransport = arrival.transport,
                )
            }
        }
        ChatEvents.notifyChatChanged(envelopeSenderUserId)
    }

    /**
     * Persist the latest relay-uploadable sealed receipt envelope for one
     * cumulative outgoing watermark. Same watermark is a no-op so the stored
     * `msg_id` stays stable; higher watermark replaces it with a newly sealed
     * envelope and clears the relay-posted marker in core.
     */
    private fun queueOutgoingReceiptForRelay(
        identity: Identity,
        contact: Contact,
        receiptType: UByte,
        ackedSenderUserId: ByteArray,
        throughLamport: ULong,
        timestamp: Long = System.currentTimeMillis(),
    ): Boolean {
        if (throughLamport == 0uL) return false
        val existing = store.outgoingReceiptEnvelope(contact.userId, ackedSenderUserId, receiptType)
        val authored = store.ensureAuthoredReceipt(
            identity,
            contact,
            ackedSenderUserId,
            receiptType,
            throughLamport,
            timestamp,
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        return existing == null || existing.throughLamport < authored.envelope.throughLamport
    }

    /**
     * [ChatViewEvents] handler: the user just opened [peerUserId]'s chat.
     * Sends a READ receipt covering everything currently stored from that
     * peer (DESIGN.md §7.2), via [sendReceiptToContact] rather than
     * [sendReceiptOnAddress] since there's no specific link this was
     * triggered from -- it goes out over whatever link [MeshRouter] can
     * currently reach the contact on, if any.
     *
     * Best-effort immediately like every receipt: if the peer isn't connected
     * right now, [sendSealedEnvelopeToContact] simply no-ops (logged at INFO).
     * The difference from the earlier milestone is that the cumulative read
     * watermark is first persisted via `recordOutgoingReceipt`, so the next
     * digest sync re-sends it receipts-first and closes the old retry gap.
     */
    private fun handleChatViewed(peerUserId: ByteArray) {
        val identity = this.identity ?: return
        val contact = store.getContact(peerUserId) ?: return
        // highestLamport (plain MAX), not highestContiguousLamport: the
        // latter counts contiguously from lamport 1 and returns 0 at the
        // first hole, but the lamport ratchet lets a peer's stream
        // legitimately start above 1 after a chat history wipe (lamports
        // below the new base never existed for anyone). A receiver holding
        // e.g. {3, 4} from that peer would get 0 from the contiguous count
        // forever, so opening the chat would never clear the unread badge
        // or advance the read tick. MAX correctly reflects what we actually
        // hold. The `== 0` guard below still means "nothing received yet,"
        // since MAX is 0 only when the store truly has no message from
        // this peer.
        val throughLamport = store.highestLamport(peerUserId, peerUserId)
        if (throughLamport == 0uL) return // nothing received from this peer yet to ack as read
        store.recordOutgoingReceipt(peerUserId, peerUserId, RECEIPT_TYPE_READ, throughLamport)
        if (
            queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = peerUserId,
                throughLamport = throughLamport,
            )
        ) {
            RelaySyncEvents.requestSync()
        }
        sendReceiptToContact(identity, contact, RECEIPT_TYPE_READ, peerUserId, throughLamport)
    }

    /** Builds a [ReceiptContent] and sends it as a sealed envelope on the exact link [address] (a reply to a frame that just arrived on it). */
    private fun sendReceiptOnAddress(
        identity: Identity,
        contact: Contact,
        address: String,
        receiptType: UByte,
        ackedSenderUserId: ByteArray,
        throughLamport: ULong,
    ) {
        val authored = store.ensureAuthoredReceipt(
            identity,
            contact,
            ackedSenderUserId,
            receiptType,
            throughLamport,
            System.currentTimeMillis(),
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        MeshRouter.sendToAddress(address, authored.frame)
    }

    /** Builds a [ReceiptContent] and sends it to whichever live link currently reaches [contact], if any -- see [handleChatViewed]. */
    private fun sendReceiptToContact(
        identity: Identity,
        contact: Contact,
        receiptType: UByte,
        ackedSenderUserId: ByteArray,
        throughLamport: ULong,
    ) {
        val authored = store.ensureAuthoredReceipt(
            identity,
            contact,
            ackedSenderUserId,
            receiptType,
            throughLamport,
            System.currentTimeMillis(),
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        if (!MeshRouter.sendToUserId(contact.userId, authored.frame)) {
            Log.i(TAG, "Receipt to ${UserIdHex.encode(contact.userId)} queued; not currently connected")
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
