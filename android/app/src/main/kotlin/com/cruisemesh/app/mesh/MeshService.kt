package com.cruisemesh.app.mesh

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.identity.IdentityStore
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.ReceiptContent
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.decodeMessageBody
import uniffi.cruisemesh_core.decodeReceiptContent
import uniffi.cruisemesh_core.encodeEnvelopeFrame
import uniffi.cruisemesh_core.encodeHello
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.encodeReceiptContent
import uniffi.cruisemesh_core.openMessage
import uniffi.cruisemesh_core.parseFrame
import uniffi.cruisemesh_core.sealMessage

private const val TAG = "MeshService"
private const val NOTIFICATION_CHANNEL_ID = "cruisemesh_mesh"
private const val NOTIFICATION_ID = 1

/** `kind` bytes from DESIGN.md §7.1. */
private const val KIND_TEXT: UByte = 1u
private const val KIND_RECEIPT: UByte = 2u

/** `receipt_type` for a "delivered" receipt (DESIGN.md §7.2); "read" is a later milestone. */
private const val RECEIPT_TYPE_DELIVERED: UByte = 1u

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
 * reason.
 */
class MeshService : Service() {

    private var identity: Identity? = null
    private lateinit var store: MessageStore

    private val peripheral by lazy {
        BlePeripheral(this, ::onFrameReceived, ::onPeripheralCentralSubscribed, ::onPeripheralCentralDisconnected)
    }
    private val central by lazy {
        BleCentral(this, ::onFrameReceived, ::onCentralPeerConnected, ::onCentralPeerDisconnected)
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(NOTIFICATION_ID, buildNotification())

        if (!hasRequiredPermissions()) {
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
            Log.e(TAG, "No identity persisted; stopping mesh service")
            stopSelf()
            return START_NOT_STICKY
        }
        identity = loadedIdentity
        store = AppStore.get(this)

        MeshRouter.registerCentral(central::sendFrame)
        MeshRouter.registerPeripheral(peripheral::sendFrame)

        peripheral.start()
        central.start()
        return START_STICKY
    }

    override fun onDestroy() {
        peripheral.stop()
        central.stop()
        MeshRouter.unregisterCentral()
        MeshRouter.unregisterPeripheral()
        // stop() above tears down connections without per-address disconnect
        // callbacks, so clear the router's mappings wholesale.
        MeshRouter.reset()
        super.onDestroy()
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
            is Frame.Envelope -> handleEnvelope(address, parsed.sealed, identity)
        }
    }

    /**
     * HELLO handling (DESIGN.md §5.2 handshake). Records the address->userId
     * mapping, then -- only for a known contact -- replays every message
     * *we* authored into that contact's chat.
     *
     * This is a deliberately naive stand-in for the real digest exchange
     * (DESIGN.md §7.3): rather than figuring out what the peer is actually
     * missing, we just resend our entire outgoing history for that chat on
     * every reconnect. That's safe because [MessageStore.insertMessage] is
     * idempotent on the receiving end (it returns `false` and stores nothing
     * for a duplicate), so replaying already-delivered messages is a wasted
     * radio write, never a correctness problem. Milestone 2 replaces this
     * with real per-chat digests (highest-contiguous-lamport + a msg_id
     * bloom filter) so only the actual gap gets resent.
     *
     * An unrecognized userId means "not a friend (yet)" -- we keep the
     * address mapping (they may friend us later this session) but send
     * nothing, since we have no key to seal anything to them with.
     */
    private fun handleHello(address: String, userId: ByteArray, identity: Identity) {
        Log.i(TAG, "HELLO from $address: userId=${UserIdHex.encode(userId)}")
        MeshRouter.onHello(address, userId)

        val contact = store.getContact(userId)
        if (contact == null) {
            Log.i(TAG, "HELLO from unrecognized userId=${UserIdHex.encode(userId)}; not replaying anything")
            return
        }

        val ownMessages = store.messagesForChat(contact.userId)
            .filter { it.kind == KIND_TEXT && it.senderUserId.contentEquals(identity.userId) }
        for (message in ownMessages) {
            sendSealedEnvelope(
                identity = identity,
                recipientAgreePk = contact.agreePk,
                address = address,
                kind = KIND_TEXT,
                lamport = message.lamport,
                timestamp = message.timestamp,
                content = message.payload,
            )
        }
    }

    /**
     * Envelope handling (DESIGN.md §6.3 open/verify, §7.1 body layout). See
     * this class's KDoc for why `body.chatId == opened.senderUserId` is the
     * correct sanity check here.
     */
    private fun handleEnvelope(address: String, sealed: ByteArray, identity: Identity) {
        val opened = try {
            openMessage(identity, sealed)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping envelope from $address: failed to open (${e.message})")
            return
        }
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
            KIND_TEXT -> handleIncomingText(address, opened.senderUserId, body, identity)
            KIND_RECEIPT -> handleIncomingReceipt(address, body)
            else -> Log.i(TAG, "Dropping envelope from $address: unhandled kind=${body.kind}")
        }
    }

    /**
     * Stores an incoming text message and, only if it was newly inserted,
     * sends a delivered receipt back on the same link (DESIGN.md §7.2). A
     * duplicate (e.g. re-sent by the peer's own HELLO replay above) is a
     * silent no-op here -- it was already acknowledged the first time, and
     * re-acking it wouldn't change anything, so this path can never send two
     * receipts for one message.
     *
     * This never triggers another receipt (see [handleIncomingReceipt]):
     * receipts are kind=2, this branch only ever runs for kind=1, and
     * [handleIncomingReceipt] never calls back into this method. Combined
     * with [handleHello] only ever replaying kind=1 messages we authored,
     * there's no cycle where a receipt causes a receipt.
     */
    private fun handleIncomingText(address: String, senderUserId: ByteArray, body: MessageBody, identity: Identity) {
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_TEXT,
                payload = body.content,
            ),
        )
        if (!inserted) return
        ChatEvents.notifyChatChanged(senderUserId)

        val contact = store.getContact(senderUserId)
        if (contact == null) {
            // We stored the message (friending can happen independently of
            // messaging order), but with no contact we have no agreePk to
            // seal a receipt to, so skip it.
            Log.i(TAG, "Stored a message from unrecognized userId=${UserIdHex.encode(senderUserId)}; no receipt to send")
            return
        }

        val throughLamport = store.highestContiguousLamport(senderUserId, senderUserId)
        val receipt = ReceiptContent(
            chatId = identity.userId, // wire convention: chatId = this envelope's sender, i.e. us -- see class KDoc
            senderUserId = senderUserId, // whose messages are being acknowledged
            lamport = throughLamport,
            receiptType = RECEIPT_TYPE_DELIVERED,
        )
        sendSealedEnvelope(
            identity = identity,
            recipientAgreePk = contact.agreePk,
            address = address,
            kind = KIND_RECEIPT,
            // Receipts are not part of a chat's lamport stream (that's for
            // messages, DESIGN.md §7.1) and are never persisted on either
            // side -- lamport=0 here is deliberate filler, not a real
            // sequence number. The actual cumulative "delivered through N"
            // value lives in ReceiptContent.lamport above.
            lamport = 0uL,
            timestamp = System.currentTimeMillis(),
            content = encodeReceiptContent(receipt),
        )
    }

    /**
     * Logs an incoming receipt. Receipt persistence and turning it into a
     * chat's ✓✓ tick (DESIGN.md §7.2) is UI/store work for a later step --
     * this only proves the wire round-trip for now.
     */
    private fun handleIncomingReceipt(address: String, body: MessageBody) {
        val receipt = try {
            decodeReceiptContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping receipt from $address: failed to decode (${e.message})")
            return
        }
        Log.i(
            TAG,
            "Receipt from $address: ackedSender=${UserIdHex.encode(receipt.senderUserId)} " +
                "throughLamport=${receipt.lamport} type=${receipt.receiptType}",
        )
    }

    /** Builds, seals, and sends one [MessageBody] as an envelope frame on [address]. */
    private fun sendSealedEnvelope(
        identity: Identity,
        recipientAgreePk: ByteArray,
        address: String,
        kind: UByte,
        lamport: ULong,
        timestamp: Long,
        content: ByteArray,
    ) {
        val body = MessageBody(
            kind = kind,
            chatId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            content = content,
        )
        val sealed = try {
            sealMessage(identity, recipientAgreePk, encodeMessageBody(body))
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to seal outgoing kind=$kind frame for $address: ${e.message}")
            return
        }
        MeshRouter.sendToAddress(address, encodeEnvelopeFrame(sealed))
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
            .setContentText("Relaying messages nearby")
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setOngoing(true)
            .build()
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
