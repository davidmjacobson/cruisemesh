package com.cruisemesh.app.mesh

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
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
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.KIND_REACTION
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.MessageNotifier
import com.cruisemesh.app.relay.RelayClient
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayImport
import uniffi.cruisemesh_core.CarriedEnvelope
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.DigestEntry
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OpenedMessage
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.ReceiptContent
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.decodeGroupInviteContent
import uniffi.cruisemesh_core.decodeMessageBody
import uniffi.cruisemesh_core.decodeReceiptContent
import uniffi.cruisemesh_core.defaultExpiry
import uniffi.cruisemesh_core.encodeDigest
import uniffi.cruisemesh_core.encodeEnvelopeFrame
import uniffi.cruisemesh_core.encodeHello
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.encodeReceiptContent
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.openGroupMessage
import uniffi.cruisemesh_core.parseFriendCard
import uniffi.cruisemesh_core.openMessage
import uniffi.cruisemesh_core.parseFrame
import uniffi.cruisemesh_core.sealMessage

private const val TAG = "MeshService"
private const val NOTIFICATION_CHANNEL_ID = "cruisemesh_mesh"
private const val NOTIFICATION_ID = 1

/** `kind` bytes from DESIGN.md §7.1. */
private const val KIND_TEXT: UByte = 1u
private const val KIND_RECEIPT: UByte = 2u
private const val KIND_FRIEND_REQUEST: UByte = 3u
private const val KIND_GROUP_INVITE: UByte = 4u

/** `receipt_type` values (DESIGN.md §7.2): delivered = recipient decrypted and stored it, read = recipient viewed the chat. */
private const val RECEIPT_TYPE_DELIVERED: UByte = 1u
private const val RECEIPT_TYPE_READ: UByte = 2u

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

/** DESIGN.md §5.3: the bounded budget (~5 MB) of *foreign* muled envelopes; family (known-recipient) traffic is exempt. */
private const val FOREIGN_CARRY_BUDGET_BYTES: Long = 5L * 1024 * 1024
private const val RELAY_BATCH_LIMIT: ULong = 128uL
private const val RELAY_POLL_INTERVAL_MS = 60_000L

/**
 * Exact carried-`msg_id` count advertised in the interim digest. This is the
 * stand-in for §7.3's deferred bloom filter: large enough to suppress blind
 * resend of a typical family-scale carry queue, still small enough to fit in
 * one HELLO sync over fragmented BLE.
 */
private const val DIGEST_CARRIED_MSG_IDS_LIMIT: ULong = 512uL

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

    private var identity: Identity? = null
    private lateinit var store: MessageStore
    private var running = false
    private var meshRolesRunning = false
    private var bluetoothAudioConnected = false
    private var bluetoothAudioReceiverRegistered = false
    private var bluetoothStateReceiverRegistered = false
    private var relayNetworkCallbackRegistered = false
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
    private val a2dpAudioBackoff = A2dpAudioBackoff()

    private val peripheral by lazy {
        BlePeripheral(this, ::onFrameReceived, ::onPeripheralCentralSubscribed, ::onPeripheralCentralDisconnected)
    }
    private val central by lazy {
        BleCentral(this, ::onFrameReceived, ::onCentralPeerConnected, ::onCentralPeerDisconnected)
    }
    private val bluetoothAudioReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            val action = intent?.action ?: return
            val state = intent.getIntExtra(BluetoothProfile.EXTRA_STATE, -1)
            refreshBluetoothAudioStatus("$action state=$state")
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
        }

        override fun onLost(network: Network) {
            if (relayBindNetwork == network) relayBindNetwork = null
        }
    }
    private val relayPollRunnable = object : Runnable {
        override fun run() {
            requestRelaySync("poll interval")
            relayMainHandler.postDelayed(this, RELAY_POLL_INTERVAL_MS)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
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

        MeshRouter.registerCentral(central::sendFrame)
        MeshRouter.registerPeripheral(peripheral::sendFrame)
        ChatViewEvents.register(::handleChatViewed)
        RelaySyncEvents.register { requestRelaySync("queue changed") }

        running = true
        registerBluetoothAudioReceiver()
        registerBluetoothStateReceiver()
        registerRelayNetworkCallback()
        scheduleRelayPolling()
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
        return START_STICKY
    }

    override fun onDestroy() {
        running = false
        MeshRuntimeStatus.markStopped()
        unregisterBluetoothAudioReceiver()
        unregisterBluetoothStateReceiver()
        unregisterRelayNetworkCallback()
        cancelRelayPolling()
        RelaySyncEvents.unregister()
        stopMeshRoles()
        MeshRouter.unregisterCentral()
        MeshRouter.unregisterPeripheral()
        ChatViewEvents.unregister()
        // stop() above tears down connections without per-address disconnect
        // callbacks, so clear the router's mappings wholesale.
        MeshRouter.reset()
        super.onDestroy()
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
        // stop() tears links down without per-address disconnect callbacks.
        MeshRouter.reset()
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
        relayMainHandler.removeCallbacks(relayPollRunnable)
        relayMainHandler.postDelayed(relayPollRunnable, RELAY_POLL_INTERVAL_MS)
    }

    private fun cancelRelayPolling() {
        relayMainHandler.removeCallbacks(relayPollRunnable)
    }

    private fun requestRelaySync(reason: String) {
        if (!running || identity == null || !hasValidatedInternet()) return
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
        if (configs.isEmpty()) return
        for (config in configs) {
            pollRelayMailbox(config, identity, contacts, fallbackConfig, now, network)
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
        for (envelope in store.pendingRelayOutgoingReceiptEnvelopes(RELAY_BATCH_LIMIT, now)) {
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
        for (envelope in store.pendingRelayOutboundEnvelopes(RELAY_BATCH_LIMIT, now)) {
            // 1:1 / invite envelopes are addressed to a contact userId; group
            // text uses recipientUserId = group.id and rides the family's
            // fallback (or any member's) relay config.
            val contact = contactsByUserId[UserIdHex.encode(envelope.recipientUserId)]
            val config = if (contact != null) {
                resolvedRelayConfig(contact, fallbackConfig)
            } else {
                relayConfigForGroupRecipient(envelope.recipientUserId, contacts, fallbackConfig)
            } ?: continue
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
        for (envelope in store.familyCarriedEnvelopes(RELAY_BATCH_LIMIT, now)) {
            val contact = contactMatchingHint(contacts, envelope.recipientHint, now) ?: continue
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
     * Fetches this config's relay mailbox and, per [InboundDisposition],
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
     * [InboundDisposition] ([shouldAck]) instead of unconditionally acking
     * everything the fetch returned. A proxied envelope comes back as
     * CARRIED, not CONSUMED, so it is deliberately left on the relay --
     * [MeshService.carryRelayEnvelope] already queued it for BLE delivery to
     * its real recipient, and the relay copy remains the durable fallback
     * until they (or another proxy) fetch and consume it, or it expires.
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
        while (running && hasValidatedInternet()) {
            val page = RelayClient.fetchEnvelopes(config, hints, after, RELAY_BATCH_LIMIT.toInt(), network)
            Log.i(
                TAG,
                "Fetched ${page.envelopes.size} relay envelope(s) from ${config.relayUrl} after=$after next=${page.nextCursor}",
            )
            if (page.envelopes.isEmpty()) return
            val ackIds = ArrayList<Long>(page.envelopes.size)
            for (envelope in page.envelopes) {
                val disposition = handleRelayEnvelope(envelope, identity)
                if (shouldAck(disposition)) {
                    ackIds += envelope.id
                }
            }
            if (ackIds.isNotEmpty()) {
                Log.i(TAG, "Acking ${ackIds.size} relay envelope(s) on ${config.relayUrl}: $ackIds")
                RelayClient.ackEnvelopes(config, ackIds, network)
            }
            after = page.nextCursor
            if (page.envelopes.size < RELAY_BATCH_LIMIT.toInt()) return
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
        val relayUrl = contact.relayUrl?.trim().orEmpty()
        val relayToken = contact.relayToken?.trim().orEmpty()
        if (relayUrl.isNotEmpty() && relayToken.isNotEmpty()) {
            return RelayConfig(relayUrl, relayToken)
        }
        return fallbackConfig
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
    private fun refreshBluetoothAudioStatus(reason: String) {
        val connected = when (a2dpAudioBackoff.update(isA2dpConnected())) {
            A2dpAudioBackoff.Mode.ACTIVE -> false
            A2dpAudioBackoff.Mode.PAUSED_FOR_A2DP -> true
            null -> return
        }
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
    }

    private fun onCentralPeerDisconnected(address: String) {
        MeshRouter.onDisconnected(address)
    }

    private fun onPeripheralCentralSubscribed(address: String) {
        MeshRouter.onConnected(address, MeshRouterState.Transport.PERIPHERAL)
        sendHello(address)
    }

    private fun onPeripheralCentralDisconnected(address: String) {
        MeshRouter.onDisconnected(address)
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
        }
    }

    /**
     * HELLO handling (DESIGN.md §5.2 handshake). Records the address->userId
     * mapping, then kicks off the real digest sync (DESIGN.md §7.3). Every
     * peer, contact or stranger, gets a digest now because the carried
     * `msg_id` set is useful to both: it suppresses blind re-spray of foreign
     * mule traffic on reconnect. A known contact additionally gets the
     * per-sender lamport digest for the 1:1 chat, i.e. "here's what I have
     * from myself, contiguously, through lamport N per sender." That's the
     * wire-chatId convention from the class KDoc applied to DIGEST frames:
     * `chatId` here is OUR OWN userId, and `entries` is
     * [MessageStore.chatDigest] keyed by the *local* chat (the contact's
     * userId), because locally that's how this 1:1 chat's history is stored.
     * The peer's [handleDigest] uses the matching digest we sent it (from a
     * prior HELLO) the same way to send us what we're missing -- see that
     * method for the receiving half of this exchange. This replaces the
     * earlier naive stand-in that just resent our entire outgoing history on
     * every reconnect.
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
        MeshRouter.onHello(address, userId)
        Log.i(TAG, "HELLO from $address: userId=${UserIdHex.encode(userId)}")

        // Hand off anything we're muling for this peer (DESIGN.md §5.3 carry
        // queue) before the digest sync. This runs for *any* peer, contact or
        // not: we carry foreign envelopes for strangers too, and a stranger to
        // us may still be the intended recipient of something we picked up.
        drainCarriedEnvelopesTo(address, userId)

        val contact = store.getContact(userId)
        val digestEntries = contact?.let { store.chatDigest(it.userId) } ?: emptyList()
        if (contact == null) {
            Log.i(TAG, "HELLO from unrecognized userId=${UserIdHex.encode(userId)}; sending carry-suppression digest only")
        }
        val digestFrame = encodeDigest(identity.userId, digestEntries, store.carriedMsgIds(DIGEST_CARRIED_MSG_IDS_LIMIT))
        MeshRouter.sendToAddress(address, digestFrame)
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
        sprayCarriedEnvelopesTo(address, resolvedPeerUserId, recentMsgIds)
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
     * DESIGN.md §5.3 carry follow-up: spray carried foreign envelopes onward
     * to a non-recipient mule on reconnect, excluding anything actually
     * destined for that peer (the targeted drain already handled those) and
     * anything the peer's digest says it already knows by `msg_id`.
     *
     * Unlike [drainCarriedEnvelopesTo], this keeps the local copy: a mule
     * stays a mule until the true recipient eventually opens it.
     */
    private fun sprayCarriedEnvelopesTo(address: String, peerUserId: ByteArray, peerKnownMsgIds: List<ByteArray>) {
        val now = System.currentTimeMillis()
        try {
            store.pruneExpiredCarried(now)
            val peerHints = recentHintsFor(peerUserId, now)
            val toSpray = store.carriedEnvelopesForPeerSync(peerHints, peerKnownMsgIds, now)
            if (toSpray.isEmpty()) return
            var sprayed = 0
            for (env in toSpray) {
                val frame = encodeEnvelopeFrame(env.msgId, env.hopTtl, env.expiry, env.recipientHint, env.sealed)
                if (MeshRouter.sendToAddress(address, frame)) {
                    sprayed++
                }
            }
            Log.i(TAG, "Sprayed $sprayed carried envelope(s) to mule $address")
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to spray carried envelopes to $address: ${e.message}")
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
     * [processInboundEnvelope] now returns an [InboundDisposition] so
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
    ): InboundDisposition {
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
     * ([handleRelayEnvelope]), non-null means it arrived over a live BLE link
     * ([handleEnvelope]). The two foreign-carry branches below use that to
     * pick [carryRelayEnvelope] (durable, never re-uploaded -- it's already on
     * the relay) vs. the existing [carryForeignEnvelope] (durable-if-family,
     * uploaded to the relay so an internet phone can proxy it onward) for
     * envelopes we can't open ourselves. See [InboundDisposition] for what
     * each return value means to the caller.
     */
    private fun processInboundEnvelope(
        sourceAddress: String?,
        envelope: Frame.Envelope,
        identity: Identity,
    ): InboundDisposition {
        val sourceLabel = sourceAddress ?: "relay"
        if (!GossipState.seenIds.checkAndRecord(envelope.msgId)) {
            // Already handled this msg_id; a redundant copy from the flood.
            return InboundDisposition.SEEN
        }
        if (envelope.expiry <= System.currentTimeMillis()) {
            Log.i(TAG, "Dropping expired envelope from $sourceLabel (expiry=${envelope.expiry})")
            return InboundDisposition.EXPIRED
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
                deliverOpenedGroupEnvelope(sourceLabel, groupOpened.first, groupOpened.second, identity)
                relayForeignEnvelope(sourceAddress, envelope)
                if (sourceAddress == null) {
                    carryRelayEnvelope(envelope)
                } else {
                    carryForeignEnvelope(envelope, forceFamily = true)
                }
                return InboundDisposition.CONSUMED
            }
            // Not for us (or unopenable) -> foreign traffic. Two jobs, both
            // best-effort (DESIGN.md §5.3): flood it to whoever's connected
            // right now, and carry it so we can hand it to its recipient the
            // next time we meet them, even if that's hours from now.
            relayForeignEnvelope(sourceAddress, envelope)
            if (sourceAddress == null) {
                carryRelayEnvelope(envelope)
            } else {
                carryForeignEnvelope(envelope)
            }
            return InboundDisposition.CARRIED
        }
        deliverOpenedEnvelope(sourceLabel, opened, identity)
        return InboundDisposition.CONSUMED
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
     * family envelopes are kept until expiry and never evicted for space,
     * while foreign ones share a bounded [FOREIGN_CARRY_BUDGET_BYTES] budget.
     * Idempotent on `msg_id`, so re-seeing an envelope we already carry is a
     * no-op. Reached only after [handleEnvelope]'s dedupe + expiry gates, so
     * we never carry a stale duplicate or an already-expired envelope.
     */
    private fun carryForeignEnvelope(envelope: Frame.Envelope, forceFamily: Boolean = false) {
        val now = System.currentTimeMillis()
        try {
            val isFamily = forceFamily || hintMatchesKnownTarget(envelope.recipientHint, now)
            val stored = store.enqueueCarriedEnvelope(
                CarriedEnvelope(
                    msgId = envelope.msgId,
                    hopTtl = envelope.hopTtl,
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
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to enqueue carried envelope: ${e.message}")
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
     */
    private fun carryRelayEnvelope(envelope: Frame.Envelope) {
        val now = System.currentTimeMillis()
        try {
            val stored = store.enqueueRelayCarriedEnvelope(
                CarriedEnvelope(
                    msgId = envelope.msgId,
                    hopTtl = envelope.hopTtl,
                    expiry = envelope.expiry,
                    recipientHint = envelope.recipientHint,
                    sealed = envelope.sealed,
                ),
                now,
            )
            if (stored) {
                Log.i(TAG, "Carrying relay-sourced envelope (proxy) for later BLE delivery")
            }
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to enqueue relay-carried envelope: ${e.message}")
        }
    }

    /**
     * Hands over every carried envelope destined for the peer that just
     * HELLO'd on [address] (DESIGN.md §5.3): we compute the peer's recent-day
     * `recipient_hint`s ([recentHintsFor]) and pull matching envelopes from
     * the store, send each on this link, and drop it once sent (a mule's job
     * ends on delivery). Expired entries are pruned first. If the peer already
     * saw an envelope via an earlier flood, their own seen-ID set drops the
     * duplicate harmlessly; if they didn't (the whole point -- they were out
     * of range when it flooded), this is how it reaches them.
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
                    // Keep group carries so we can still mule to other members;
                    // only drop true 1:1-targeted envelopes once handed over.
                    if (!recognizesGroupHint(env.recipientHint, now)) {
                        store.removeCarriedEnvelope(env.msgId)
                    }
                    delivered++
                }
            }
            Log.i(TAG, "Drained $delivered carried envelope(s) to $address")
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

    private fun recognizesGroupHint(hint: ByteArray, now: Long): Boolean {
        for (group in store.listGroups()) {
            if (recentHintsFor(group.id, now).any { it.contentEquals(hint) }) {
                return true
            }
        }
        return false
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

    /**
     * Floods a foreign (not-for-us) envelope onward per DESIGN.md §5.3, if it
     * still has hop budget. `hop_ttl` is the remaining number of hops; we
     * decrement it and forward only while at least one hop would remain
     * (`hop_ttl > 1`), so a frame arriving with `hop_ttl == 1` is the last
     * carrier's copy and stops here. The `msg_id`, `expiry`, `recipient_hint`,
     * and sealed bytes are all preserved verbatim -- only `hop_ttl` changes --
     * so every carrier along the way computes the same dedupe key. The
     * arriving link is excluded from the flood to avoid the trivial echo; the
     * seen-ID set (already updated by [handleEnvelope]) stops longer loops.
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
    private fun deliverOpenedEnvelope(address: String, opened: OpenedMessage, identity: Identity) {
        val body = try {
            decodeMessageBody(opened.payload)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping envelope from $address: failed to decode body (${e.message})")
            return
        }
        if (!body.chatId.contentEquals(opened.senderUserId)) {
            Log.w(TAG, "Dropping envelope from $address: chatId does not match the verified sender")
            return
        }

        when (body.kind) {
            KIND_TEXT -> handleIncomingChatMessage(address, opened.senderUserId, body, identity, KIND_TEXT)
            KIND_ATTACHMENT_MANIFEST -> handleIncomingChatMessage(
                address,
                opened.senderUserId,
                body,
                identity,
                KIND_ATTACHMENT_MANIFEST,
            )
            KIND_REACTION -> handleIncomingChatMessage(address, opened.senderUserId, body, identity, KIND_REACTION)
            KIND_RECEIPT -> handleIncomingReceipt(address, opened.senderUserId, body, identity)
            KIND_FRIEND_REQUEST -> handleIncomingFriendRequest(address, opened.senderUserId, body, identity)
            KIND_GROUP_INVITE -> handleIncomingGroupInvite(address, opened.senderUserId, body, identity)
            else -> Log.i(TAG, "Dropping envelope from $address: unhandled kind=${body.kind}")
        }
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

        val body = try {
            decodeMessageBody(opened.payload)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping group envelope from $address: failed to decode body (${e.message})")
            return
        }
        if (!body.chatId.contentEquals(group.id)) {
            Log.w(TAG, "Dropping group envelope from $address: body.chatId does not match group id")
            return
        }
        when (body.kind) {
            KIND_TEXT -> handleIncomingGroupChatMessage(address, group, opened.senderUserId, body)
            KIND_REACTION -> handleIncomingGroupChatMessage(address, group, opened.senderUserId, body)
            else -> Log.i(TAG, "Dropping group envelope from $address: unhandled kind=${body.kind}")
        }
    }

    private fun handleIncomingGroupChatMessage(
        address: String,
        group: Group,
        senderUserId: ByteArray,
        body: MessageBody,
    ) {
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = group.id,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = body.kind,
                payload = body.content,
            ),
        )
        if (!inserted) {
            Log.i(
                TAG,
                "Ignoring duplicate group kind=${body.kind} from $address " +
                    "sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
            )
            return
        }
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
            val senderName = store.getContact(senderUserId)?.name
                ?: UserIdHex.encode(senderUserId).take(8)
            val preview = body.content.toString(Charsets.UTF_8)
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
            val senderName = contact?.name ?: UserIdHex.encode(senderUserId).take(8)
            MessageNotifier.notifyIncomingGroupMessage(
                this,
                group,
                senderName,
                "Added you to ${group.name}",
            )
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
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
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
        store.upsertContact(contact)
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
        Log.i(TAG, "Imported contact ${contact.name} from friend request on $address")
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
    ) {
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = kind,
                payload = body.content,
            ),
        )
        if (!inserted) {
            Log.i(
                TAG,
                "Ignoring duplicate kind=$kind from $address sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
            )
            return
        }
        Log.i(
            TAG,
            "Stored kind=$kind from $address sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
        )
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
    private fun handleIncomingReceipt(address: String, envelopeSenderUserId: ByteArray, body: MessageBody, identity: Identity) {
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
        store.recordReceipt(
            chatId = envelopeSenderUserId, // local convention: chat keyed by the other party -- see class KDoc
            senderUserId = identity.userId, // whose messages this receipt is about: ours
            receiptType = receipt.receiptType,
            throughLamport = receipt.lamport,
        )
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
        if (existing != null && existing.throughLamport >= throughLamport) {
            return false
        }
        val envelope = buildOutgoingReceiptEnvelope(
            identity = identity,
            contact = contact,
            receiptType = receiptType,
            ackedSenderUserId = ackedSenderUserId,
            throughLamport = throughLamport,
            timestamp = timestamp,
        ) ?: return false
        return store.upsertOutgoingReceiptEnvelope(envelope, timestamp)
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
        sendSealedEnvelope(
            identity = identity,
            recipientUserId = contact.userId,
            recipientAgreePk = contact.agreePk,
            address = address,
            kind = KIND_RECEIPT,
            // Receipts are not part of a chat's lamport stream (that's for
            // messages, DESIGN.md §7.1) and are never persisted on either
            // side -- lamport=0 here is deliberate filler, not a real
            // sequence number. The actual cumulative "delivered/read through
            // N" value lives in ReceiptContent.lamport below.
            lamport = 0uL,
            timestamp = System.currentTimeMillis(),
            content = encodeReceiptContent(
                ReceiptContent(
                    chatId = identity.userId, // wire convention: chatId = this envelope's sender, i.e. us -- see class KDoc
                    senderUserId = ackedSenderUserId, // whose messages are being acknowledged
                    lamport = throughLamport,
                    receiptType = receiptType,
                ),
            ),
        )
    }

    /** Builds a [ReceiptContent] and sends it to whichever live link currently reaches [contact], if any -- see [handleChatViewed]. */
    private fun sendReceiptToContact(
        identity: Identity,
        contact: Contact,
        receiptType: UByte,
        ackedSenderUserId: ByteArray,
        throughLamport: ULong,
    ) {
        sendSealedEnvelopeToContact(
            identity = identity,
            contact = contact,
            kind = KIND_RECEIPT,
            lamport = 0uL, // see sendReceiptOnAddress's comment: deliberate filler, not a sequence number
            timestamp = System.currentTimeMillis(),
            content = encodeReceiptContent(
                ReceiptContent(
                    chatId = identity.userId,
                    senderUserId = ackedSenderUserId,
                    lamport = throughLamport,
                    receiptType = receiptType,
                ),
            ),
        )
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
        val outbound = buildOutboundAuthoredEnvelope(identity, contact, message) ?: return null
        store.insertOutgoingMessage(message, outbound, message.timestamp)
        return outbound
    }

    /** Sends one previously persisted outbound envelope on the exact link [address]. */
    private fun sendStoredOutboundEnvelope(address: String, envelope: OutboundEnvelope) {
        MeshRouter.sendToAddress(address, encodeOutboundEnvelopeFrame(envelope))
    }

    /**
     * Seals one [MessageBody] into an envelope frame, or null (logged) if
     * sealing fails. Wraps the sealed bytes in the §6.4 public header: a
     * fresh random `msgId`, `DEFAULT_HOP_TTL`, an expiry `DEFAULT_EXPIRY_MS`
     * out from now, and a `recipientHint` for [recipientUserId] as of now.
     * This helper remains for auto-generated receipts; authored text now uses
     * the persistent outbound-envelope queue instead so reconnect retries and
     * relay upload preserve one stable `msg_id` and ciphertext per message.
     */
    private fun sealEnvelopeFrame(
        identity: Identity,
        recipientUserId: ByteArray,
        recipientAgreePk: ByteArray,
        kind: UByte,
        lamport: ULong,
        timestamp: Long,
        content: ByteArray,
    ): ByteArray? {
        val body = MessageBody(
            kind = kind,
            chatId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            content = content,
        )
        return try {
            val now = System.currentTimeMillis()
            val msgId = generateMsgId()
            // Remember our own msg_id so that if a relay floods this envelope
            // back to us we recognise it as a duplicate rather than "foreign"
            // (we can't open our own sealed box) and re-relay it (DESIGN.md §5.3).
            GossipState.seenIds.record(msgId)
            encodeEnvelopeFrame(
                msgId,
                DEFAULT_HOP_TTL,
                defaultExpiry(now),
                computeRecipientHint(recipientUserId, now),
                sealMessage(identity, recipientAgreePk, encodeMessageBody(body)),
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to seal outgoing kind=$kind frame: ${e.message}")
            null
        }
    }

    /** Builds, seals, and sends one [MessageBody] as an envelope frame on the exact link [address]. */
    private fun sendSealedEnvelope(
        identity: Identity,
        recipientUserId: ByteArray,
        recipientAgreePk: ByteArray,
        address: String,
        kind: UByte,
        lamport: ULong,
        timestamp: Long,
        content: ByteArray,
    ) {
        val frame = sealEnvelopeFrame(identity, recipientUserId, recipientAgreePk, kind, lamport, timestamp, content) ?: return
        MeshRouter.sendToAddress(address, frame)
    }

    /**
     * Builds, seals, and sends one [MessageBody] as an envelope frame to
     * whichever live link [MeshRouter] currently has for [contact], if any
     * (unlike [sendSealedEnvelope], which targets a specific already-known
     * address). Used where the caller has a contact but not a triggering
     * frame/address, e.g. [handleChatViewed]'s read-on-view receipt.
     */
    private fun sendSealedEnvelopeToContact(
        identity: Identity,
        contact: Contact,
        kind: UByte,
        lamport: ULong,
        timestamp: Long,
        content: ByteArray,
    ) {
        val frame = sealEnvelopeFrame(identity, contact.userId, contact.agreePk, kind, lamport, timestamp, content) ?: return
        if (!MeshRouter.sendToUserId(contact.userId, frame)) {
            Log.i(TAG, "kind=$kind message to ${UserIdHex.encode(contact.userId)} stays local; not currently connected")
        }
    }

    private fun hasRequiredPermissions(): Boolean {
        val permissions = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            listOf(
                Manifest.permission.BLUETOOTH_SCAN,
                Manifest.permission.BLUETOOTH_ADVERTISE,
                Manifest.permission.BLUETOOTH_CONNECT,
            )
        } else {
            emptyList()
        }
        return permissions.all {
            ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED
        }
    }

    private fun buildNotification(): Notification {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                NOTIFICATION_CHANNEL_ID,
                "CruiseMesh mesh sync",
                NotificationManager.IMPORTANCE_LOW,
            )
            getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        }
        return NotificationCompat.Builder(this, NOTIFICATION_CHANNEL_ID)
            .setContentTitle("CruiseMesh")
            .setContentText(
                when {
                    !isBluetoothOn() -> "Paused — turn on Bluetooth to sync nearby"
                    bluetoothAudioConnected -> "Relaying messages nearby (Bluetooth audio also connected)"
                    else -> "Relaying messages nearby"
                },
            )
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setOngoing(true)
            .build()
    }

    private fun refreshForegroundNotification() {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, buildNotification())
    }

    companion object {
        /** Permissions MeshService needs before it will start its BLE roles. */
        fun requiredPermissions(): Array<String> {
            val base = mutableListOf<String>()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                base += Manifest.permission.BLUETOOTH_SCAN
                base += Manifest.permission.BLUETOOTH_ADVERTISE
                base += Manifest.permission.BLUETOOTH_CONNECT
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                base += Manifest.permission.POST_NOTIFICATIONS
            }
            return base.toTypedArray()
        }
    }
}
