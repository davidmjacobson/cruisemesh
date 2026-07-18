package com.cruisemesh.app.mesh

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattServer
import android.bluetooth.BluetoothGattServerCallback
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.bluetooth.le.BluetoothLeAdvertiser
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import android.util.Log
import java.util.ArrayDeque

private const val TAG = "BlePeripheral"

// A whole new *frame* (not fragment) only gets paced -- see
// sendNextQueuedFragment -- once this many more whole frames are already
// queued behind it for the same address. Small on purpose: the common case
// (one or two frames in flight) must still go out back-to-back with no added
// latency; pacing only kicks in for the bursty on-HELLO spray
// (drainCarriedEnvelopesTo + digest frames, DESIGN.md §5.3/§7.3) that can
// queue 19+ frames for one address at once.
private const val FRAME_PACING_DEEP_QUEUE_THRESHOLD = 3

// Modest on purpose -- just enough to let the BLE controller drain its
// congestion window between frames; not a real backoff.
private const val FRAME_PACING_DELAY_MS = 20L

/**
 * Pure decision behind [BlePeripheral]'s frame-start pacing, extracted so it
 * is unit-testable without any Android/BLE dependency: pace only when the
 * fragment about to be sent is a new frame's first ([startingNewFrame]) AND
 * at least [threshold] more whole frames are already waiting behind it.
 * Fragments continuing an already-started frame are never paced.
 */
internal fun shouldPaceFrameStart(
    startingNewFrame: Boolean,
    queuedFrames: Int,
    threshold: Int = FRAME_PACING_DEEP_QUEUE_THRESHOLD,
): Boolean = startingNewFrame && queuedFrames >= threshold

/**
 * GATT-server (peripheral) half of the dual BLE role described in
 * DESIGN.md §5.2: advertises the CruiseMesh service UUID and exposes a write
 * characteristic (inbound frames) and a notify characteristic (outbound
 * frames). Frame parsing/dedupe/sync is not wired up yet — this is
 * Milestone 0 transport plumbing only; MeshService owns permission checks
 * before calling start().
 *
 * Frames larger than one ATT notification are fragmented per DESIGN.md
 * §5.2, using each central's own negotiated MTU (from [onMtuChanged]) and a
 * per-peer send queue — a GATT server also allows only one in-flight
 * notification per connection.
 *
 * Milestone 1 (DESIGN.md §5.2, §7.3): [onFrameReceived] now carries the
 * connecting central's device address alongside the frame bytes, so callers
 * can route replies. A peripheral can only notify a central once that
 * central has subscribed via the CCCD (see [onDescriptorWriteRequest]) --
 * [onCentralSubscribed] fires at exactly that point, which is when
 * MeshService sends its half of the HELLO handshake. [onCentralDisconnected]
 * fires so callers can drop a stale address mapping.
 *
 * Link-death hardening (2026-07-10 silent-blackhole bug): live logs showed a
 * peer's supervision-timeout death on one side (status=147) going completely
 * unnoticed on the other -- [onNotificationSent] used to ignore its `status`
 * and just send the next queued fragment, so a central that had already
 * dropped the link kept looking "connected" here forever. MeshRouter kept
 * the address mapped and `sendToUserId` kept returning true while every
 * frame silently evaporated. Now a failed notify tears the link down via
 * [tearDownLink] (mirrors [BleCentral]'s send-failure hardening), which
 * fires [onCentralDisconnected] so the address gets unmapped -- the
 * undelivered frame is not lost, it lives in the persistent store and
 * redelivers via digest sync on the peer's next connection.
 *
 * Notify-congestion hardening (2026-07-17 LAN-hint-loss bug): the above
 * treated *any* failed notify as proof of link death, but a burst of queued
 * frames (the on-HELLO spray below) can make the BLE controller itself
 * report transient congestion (status=129) on a link the central is still
 * actively using. [onNotificationSent] now only tears a link down after
 * [NotifyFailureTracker] sees [NotifyFailureTracker.MAX_CONSECUTIVE_FAILURES]
 * failures in a row for the same address with no success in between --
 * anything short of that retries the same fragment. [tearDownLink] itself is
 * idempotent per address so a stale device object that keeps delivering
 * queued callbacks after cleanup can never re-run the teardown or re-fire
 * [onCentralDisconnected]. Relatedly, [sendNextQueuedFragment] paces the
 * *start* of each new frame (not fragments within one) once several whole
 * frames are already queued for an address, so the on-HELLO spray itself is
 * less likely to saturate the controller in the first place -- see
 * [FRAME_PACING_DEEP_QUEUE_THRESHOLD].
 */
@SuppressLint("MissingPermission")
class BlePeripheral(
    private val context: Context,
    private val onFrameReceived: (address: String, frame: ByteArray) -> Unit,
    private val onCentralSubscribed: (String) -> Unit = {},
    private val onCentralDisconnected: (String) -> Unit = {},
) {
    private val bluetoothManager =
        context.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
    private val adapter: BluetoothAdapter? = bluetoothManager.adapter

    private var gattServer: BluetoothGattServer? = null
    private var advertiser: BluetoothLeAdvertiser? = null
    private var outboundCharacteristic: BluetoothGattCharacteristic? = null

    private val connectedDevices = mutableMapOf<String, BluetoothDevice>()
    private val negotiatedMtu = mutableMapOf<String, Int>()
    private val reassemblers = mutableMapOf<String, FrameReassembler>()

    // Queued outbound frames per address, each still split into its own
    // fragments in send order -- nested (rather than one flat fragment
    // queue) so sendNextQueuedFragment can tell when it's about to start a
    // new frame vs. continue one already in progress (see
    // FRAME_PACING_DEEP_QUEUE_THRESHOLD).
    private val notifyQueues = mutableMapOf<String, ArrayDeque<ArrayDeque<ByteArray>>>()
    private val notifyInFlight = mutableSetOf<String>()

    // Addresses whose head-of-queue frame already has >=1 fragment sent --
    // absence means the next fragment sendNextQueuedFragment picks up will
    // be a new frame's first, which is what FRAME_PACING_DEEP_QUEUE_THRESHOLD
    // gates.
    private val notifyFrameStarted = mutableSetOf<String>()

    // The exact fragment bytes currently awaiting an onNotificationSent ack
    // for an address, kept so a tolerated failure (see NotifyFailureTracker)
    // can retry the same fragment instead of silently skipping it.
    private val inFlightFragment = mutableMapOf<String, ByteArray>()
    private val notifyFailures = NotifyFailureTracker()

    // GATT server callbacks arrive on a binder thread; Handler.postDelayed's
    // callback runs on whichever Looper the Handler was built with -- use the
    // main looper so paced-send firing (and its map mutations) lands on the
    // same thread as the rest of this class's Android-framework calls
    // (mirrors BleCentral's connect-watchdog Handler).
    private val handler = Handler(Looper.getMainLooper())

    @Volatile private var advertising = false

    private val advertiseCallback = object : AdvertiseCallback() {
        override fun onStartSuccess(settingsInEffect: AdvertiseSettings) {
            advertising = true
            Log.i(TAG, "Advertising started")
        }

        override fun onStartFailure(errorCode: Int) {
            // ADVERTISE_FAILED_ALREADY_STARTED just means we're already
            // advertising (a benign restart race) -- treat it as running, not
            // a failure. Any other code means we're not advertising, so leave
            // the flag clear and a later beginAdvertising() will retry.
            advertising = errorCode == ADVERTISE_FAILED_ALREADY_STARTED
            if (errorCode != ADVERTISE_FAILED_ALREADY_STARTED) {
                Log.w(TAG, "Advertising failed: $errorCode")
            }
        }
    }

    private val gattServerCallback = object : BluetoothGattServerCallback() {
        override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
            Log.i(TAG, "Central ${device.address} connection state=$newState")
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> {
                    connectedDevices[device.address] = device
                    // Legacy connectable advertising auto-stops the instant a
                    // central connects. Without restarting it, this phone goes
                    // dark to every other peer for the rest of the process
                    // (observed live 2026-07-11: the first peer to connect took
                    // the only peripheral slot and inbound 1:1 delivery died).
                    // The stack has already stopped the advertisement, so clear
                    // the flag before restarting so beginAdvertising() actually
                    // re-arms it.
                    advertising = false
                    beginAdvertising()
                }
                BluetoothProfile.STATE_DISCONNECTED -> tearDownLink(device.address, "status=$status")
            }
        }

        override fun onMtuChanged(device: BluetoothDevice, mtu: Int) {
            Log.i(TAG, "MTU negotiated for ${device.address}: $mtu")
            negotiatedMtu[device.address] = mtu
        }

        override fun onCharacteristicWriteRequest(
            device: BluetoothDevice,
            requestId: Int,
            characteristic: BluetoothGattCharacteristic,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
            offset: Int,
            value: ByteArray,
        ) {
            Log.i(TAG, "Write request from ${device.address} for ${characteristic.uuid} (${value.size} bytes)")
            if (characteristic.uuid == MeshConstants.INBOUND_CHARACTERISTIC_UUID) {
                val reassembler = reassemblers.getOrPut(device.address) { FrameReassembler() }
                reassembler.accept(value)?.let { onFrameReceived(device.address, it) }
            }
            if (responseNeeded) {
                gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, null)
            }
        }

        override fun onDescriptorWriteRequest(
            device: BluetoothDevice,
            requestId: Int,
            descriptor: BluetoothGattDescriptor,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
            offset: Int,
            value: ByteArray,
        ) {
            // Without this override the base class never responds, so a
            // central's writeDescriptor() (used to subscribe to notifications
            // via the CCCD) hangs and eventually fails with GATT_ERROR (133).
            Log.i(TAG, "Descriptor write request from ${device.address} for ${descriptor.uuid}")
            if (responseNeeded) {
                gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, null)
            }
            val isOutboundCccdEnable = descriptor.uuid == MeshConstants.CLIENT_CONFIG_DESCRIPTOR_UUID &&
                descriptor.characteristic?.uuid == MeshConstants.OUTBOUND_CHARACTERISTIC_UUID &&
                value.contentEquals(BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE)
            if (isOutboundCccdEnable) {
                // The central has subscribed to our outbound notify
                // characteristic: this link can carry frames from us now, so
                // fire the peripheral-side half of the HELLO handshake
                // (DESIGN.md §5.2). The central's half fires symmetrically
                // from BleCentral.onDescriptorWrite once its own subscription
                // to *our* outbound characteristic completes.
                onCentralSubscribed(device.address)
            }
        }

        override fun onNotificationSent(device: BluetoothDevice, status: Int) {
            val address = device.address
            if (address !in connectedDevices) {
                // This address was already torn down -- by an earlier
                // failure in this same burst, or a raced STATE_DISCONNECTED
                // -- and the BLE stack can keep delivering queued
                // onNotificationSent callbacks for a device object after
                // cleanup. Without this guard, a single congestion burst
                // re-ran the full teardown path (including re-firing
                // onCentralDisconnected) 14 times for one address within
                // ~40ms (Pixel 10 Pro field log, 2026-07-17). Invariant:
                // once an address leaves connectedDevices, every further
                // callback for it is a no-op.
                return
            }
            if (status != BluetoothGatt.GATT_SUCCESS) {
                // Invariant: a single failed notify does NOT prove the link
                // is dead. status=129 (GATT_CONGESTED-adjacent) fired during
                // the on-HELLO spray (drainCarriedEnvelopesTo + digest
                // frames -- DESIGN.md §5.3/§7.3 -- can queue 19+ frames for
                // one address at once) while the field log showed the
                // central still writing to us on this exact address 300ms to
                // 30s later. Tearing down on the very first failure wiped
                // MeshRouter's learned address->userId mapping while the
                // link was still usable, permanently losing anything that
                // arrived afterward with nowhere to route it (e.g. a LAN
                // endpoint hint -- see MeshService.handleLanEndpointHint).
                // Retry the fragment that failed instead, and only tear the
                // link down once NotifyFailureTracker sees
                // MAX_CONSECUTIVE_FAILURES in a row for this address with no
                // success in between; a real STATE_DISCONNECTED callback
                // (below) always tears the link down regardless of this
                // count.
                notifyInFlight.remove(address)
                val retryFragment = inFlightFragment.remove(address)
                if (notifyFailures.recordFailure(address)) {
                    Log.w(
                        TAG,
                        "onNotificationSent: notify failed for $address (status=$status) " +
                            "${NotifyFailureTracker.MAX_CONSECUTIVE_FAILURES} times in a row; tearing down link",
                    )
                    gattServer?.cancelConnection(device)
                    tearDownLink(
                        address,
                        "notification send failed ${NotifyFailureTracker.MAX_CONSECUTIVE_FAILURES}x in a row (status=$status)",
                    )
                    return
                }
                Log.w(TAG, "onNotificationSent: notify failed for $address (status=$status); retrying")
                if (retryFragment != null) {
                    notifyInFlight += address
                    sendFragment(device, retryFragment)
                } else {
                    sendNextQueuedFragment(device)
                }
                return
            }
            notifyFailures.recordSuccess(address)
            notifyInFlight.remove(address)
            inFlightFragment.remove(address)
            sendNextQueuedFragment(device)
        }
    }

    fun start() {
        val btAdapter = adapter ?: run {
            Log.w(TAG, "No Bluetooth adapter; cannot start peripheral role")
            return
        }
        if (gattServer != null) {
            // Idempotence matters here: re-opening a GATT server orphans the
            // live one -- existing centrals' CCCD subscriptions live on the
            // old server object, so notifications sent via the new one never
            // reach them, and sendResponse() for requests delivered to the
            // old server goes to the wrong instance (the central then hangs
            // on its descriptor write until a ~30s supervision timeout).
            // Observed live 2026-07-08 when "Start mesh" was tapped twice.
            Log.i(TAG, "start: peripheral role already running; ignoring")
            return
        }

        gattServer = bluetoothManager.openGattServer(context, gattServerCallback)?.also { server ->
            server.addService(buildGattService())
        }

        advertiser = btAdapter.bluetoothLeAdvertiser
        beginAdvertising()
    }

    /**
     * (Re)starts connectable advertising unless it is already running.
     * Called from [start], again on every central connect (Android's legacy
     * connectable advertising stops itself the moment a connection forms), and
     * after a link tears down -- so the peripheral stays discoverable for
     * additional and subsequent centrals instead of going dark after its first
     * connection. The [advertising] guard keeps redundant calls (e.g. a
     * teardown while other links are still up) from thrashing the advertiser.
     */
    private fun beginAdvertising() {
        if (advertising) return
        // Don't advertise a torn-down server: stop() nulls gattServer, and a
        // late STATE_DISCONNECTED callback must not resurrect advertising.
        if (gattServer == null) return
        val adv = advertiser ?: return
        val settings = AdvertiseSettings.Builder()
            // BALANCED (restored from LOW_POWER 2026-07-10): the longer LOW_POWER
            // advertising interval made this peer hard for a central to catch for
            // a direct connect (status=133 churn / slow first connect). The mesh
            // no longer pauses for Bluetooth audio, so favor a faster, more
            // catchable advertisement -- re-verify earbud audio doesn't stutter.
            // TX power stays MEDIUM -- that governs range (ship-scale mesh); it's
            // the advertising *interval* (the mode) that drives coexistence.
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_BALANCED)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
            .setConnectable(true)
            .build()
        val data = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .addServiceUuid(ParcelUuid(MeshConstants.SERVICE_UUID))
            .build()
        // Carried in the scan response (not the primary advertisement) to stay
        // within the legacy 31-byte advertising budget. Lets a central
        // recognize (and skip) its own advertisement -- see
        // MeshConstants.LOCAL_INSTANCE_ID.
        val scanResponse = AdvertiseData.Builder()
            .addServiceData(ParcelUuid(MeshConstants.SERVICE_UUID), MeshConstants.LOCAL_INSTANCE_ID)
            .build()
        adv.startAdvertising(settings, data, scanResponse, advertiseCallback)
    }

    fun stop() {
        // stopAdvertising throws if the adapter is already off -- the case when
        // stop() runs because Bluetooth was turned off. Swallow it; advertising
        // is gone either way.
        advertising = false
        try {
            advertiser?.stopAdvertising(advertiseCallback)
        } catch (e: Exception) {
            Log.w(TAG, "stopAdvertising during stop() failed (adapter likely off): ${e.message}")
        }
        runCatching { gattServer?.close() }
        gattServer = null
        handler.removeCallbacksAndMessages(null)
        connectedDevices.clear()
        negotiatedMtu.clear()
        reassemblers.clear()
        notifyQueues.clear()
        notifyInFlight.clear()
        notifyFrameStarted.clear()
        inFlightFragment.clear()
        notifyFailures.clearAll()
    }

    /**
     * Push a frame to every subscribed central via the notify characteristic,
     * fragmenting per each central's negotiated MTU if needed (DESIGN.md
     * §5.2).
     */
    fun notifyFrame(frame: ByteArray) {
        connectedDevices.values.forEach { device ->
            val payloadSize = (negotiatedMtu[device.address] ?: FrameFraming.DEFAULT_ATT_MTU) -
                FrameFraming.ATT_HEADER_OVERHEAD
            val fragments = FrameFraming.fragmentOrNull(frame, payloadSize) ?: run {
                Log.w(TAG, "notifyFrame: dropping ${frame.size}-byte frame for ${device.address} -- too large to fragment")
                return@forEach
            }
            notifyQueues.getOrPut(device.address) { ArrayDeque() }.add(ArrayDeque(fragments))
            sendNextQueuedFragment(device)
        }
    }

    /**
     * Push a frame to one specific subscribed central via the notify
     * characteristic, fragmenting per that central's negotiated MTU if
     * needed (DESIGN.md §5.2) -- mirrors [BleCentral.sendFrame]. Unlike
     * [notifyFrame] (broadcast to everyone connected), this targets a single
     * peer, which is what [MeshRouter] needs to address a specific contact
     * or reply on the exact link a frame arrived on.
     */
    fun sendFrame(deviceAddress: String, frame: ByteArray) {
        val device = connectedDevices[deviceAddress] ?: run {
            Log.w(TAG, "sendFrame: no connection tracked for $deviceAddress")
            return
        }
        val payloadSize = (negotiatedMtu[deviceAddress] ?: FrameFraming.DEFAULT_ATT_MTU) -
            FrameFraming.ATT_HEADER_OVERHEAD
        val fragments = FrameFraming.fragmentOrNull(frame, payloadSize) ?: run {
            Log.w(TAG, "sendFrame: dropping ${frame.size}-byte frame for $deviceAddress -- too large to fragment")
            return
        }
        notifyQueues.getOrPut(deviceAddress) { ArrayDeque() }.add(ArrayDeque(fragments))
        Log.i(TAG, "sendFrame: queued ${fragments.size} fragment(s) for $deviceAddress (${frame.size} bytes)")
        sendNextQueuedFragment(device)
    }

    /**
     * Sends this address's next queued fragment, unless one is already
     * in-flight (a GATT server allows only one outstanding notification per
     * connection -- [notifyInFlight] enforces that one-at-a-time invariant
     * across every caller). Fragments *within* one frame always go out back
     * to back the moment the previous one is acked (chained from
     * [onNotificationSent]); only the *start* of a new frame is paced, and
     * only when several more whole frames are already queued behind it --
     * see [FRAME_PACING_DEEP_QUEUE_THRESHOLD]. Throttling changes WHEN
     * frames go out, never WHETHER: nothing here is dropped, just delayed.
     */
    private fun sendNextQueuedFragment(device: BluetoothDevice) {
        val address = device.address
        if (address in notifyInFlight) return
        val frames = notifyQueues[address] ?: return
        val currentFrame = frames.peekFirst() ?: return
        // Decide "am I about to send this frame's first fragment" BEFORE
        // polling -- notifyFrameStarted tracks whether the head frame has
        // already had >=1 fragment sent.
        val startingNewFrame = address !in notifyFrameStarted
        val fragment = currentFrame.poll() ?: run {
            // Defensive: an empty frame should never be queued (notifyFrame
            // / sendFrame only add non-empty fragment lists), but if one
            // slips through, drop it and move on rather than getting stuck.
            frames.poll()
            notifyFrameStarted.remove(address)
            return sendNextQueuedFragment(device)
        }
        if (currentFrame.isEmpty()) {
            // That was the frame's last fragment -- it's fully handed off
            // to sendFragment now, so drop it from the outer queue and reset
            // the started-marker for whatever frame comes next.
            frames.poll()
            notifyFrameStarted.remove(address)
        } else {
            notifyFrameStarted += address
        }

        // Reserve this address's one in-flight slot immediately, even when
        // the actual send below is paced -- otherwise a concurrent
        // notifyFrame()/sendFrame() call queued during the pacing delay
        // would see this address as idle and jump the queue, violating the
        // one-notification-per-connection GATT constraint.
        notifyInFlight += address
        val queuedFrames = frames.size
        if (shouldPaceFrameStart(startingNewFrame, queuedFrames)) {
            handler.postDelayed({
                if (address in connectedDevices) {
                    sendFragment(device, fragment)
                } else {
                    notifyInFlight.remove(address)
                }
            }, FRAME_PACING_DELAY_MS)
        } else {
            sendFragment(device, fragment)
        }
    }

    private fun sendFragment(device: BluetoothDevice, fragment: ByteArray) {
        val address = device.address
        val characteristic = outboundCharacteristic
        val server = gattServer
        if (characteristic == null || server == null) {
            notifyInFlight.remove(address)
            return
        }
        characteristic.value = fragment
        // notifyCharacteristicChanged throws IllegalArgumentException on an
        // oversized value; letting that unwind here would abandon the GATT
        // callback it runs on (e.g. a central never gets its write response and
        // the link times out). FrameFraming keeps fragments within the ATT cap,
        // so this guard is belt-and-suspenders against any future bad fragment.
        val notified = try {
            server.notifyCharacteristicChanged(device, characteristic, false)
        } catch (e: Exception) {
            Log.w(TAG, "sendFragment: notify threw for $address (${e.message}); dropping fragment")
            false
        }
        if (notified) {
            inFlightFragment[address] = fragment
        } else {
            // A synchronous rejection (as opposed to the async
            // onNotificationSent failure path) means the call was refused
            // outright -- e.g. no CCCD subscription -- not a transient
            // congestion status. Treated as fatal immediately, same as
            // before this file's notify-failure tolerance was added.
            Log.w(TAG, "sendFragment: notifyCharacteristicChanged rejected for $address; tearing down link")
            notifyInFlight.remove(address)
            gattServer?.cancelConnection(device)
            tearDownLink(address, "notifyCharacteristicChanged rejected")
        }
    }

    /**
     * Single per-address teardown path shared by the normal
     * STATE_DISCONNECTED callback and the notify-failure paths
     * (onNotificationSent / sendFragment) so the map cleanup and the
     * [onCentralDisconnected] signal to MeshRouter can never drift apart --
     * mirrors [BleCentral.tearDownLink] (see that class's doc comment for
     * the blackhole bug this fixes).
     *
     * Invariant: idempotent per address. [address] leaving [connectedDevices]
     * is the single source of truth for "already torn down" -- the guard
     * below means a second call for the same address (e.g. a queued
     * onNotificationSent callback the BLE stack delivers after cleanup, or a
     * STATE_DISCONNECTED racing a notify-failure teardown) is a no-op rather
     * than re-running cleanup and re-firing [onCentralDisconnected].
     */
    private fun tearDownLink(address: String, reason: String) {
        if (address !in connectedDevices) return
        Log.i(TAG, "tearDownLink: $address ($reason)")
        connectedDevices.remove(address)
        negotiatedMtu.remove(address)
        reassemblers.remove(address)
        notifyQueues.remove(address)
        notifyInFlight.remove(address)
        notifyFrameStarted.remove(address)
        inFlightFragment.remove(address)
        notifyFailures.clear(address)
        onCentralDisconnected(address)
        // A link just dropped; make sure we're advertising again so this peer
        // stays reachable. No-ops if advertising is already up.
        beginAdvertising()
    }

    private fun buildGattService(): BluetoothGattService {
        val service = BluetoothGattService(
            MeshConstants.SERVICE_UUID,
            BluetoothGattService.SERVICE_TYPE_PRIMARY,
        )

        val inbound = BluetoothGattCharacteristic(
            MeshConstants.INBOUND_CHARACTERISTIC_UUID,
            BluetoothGattCharacteristic.PROPERTY_WRITE,
            BluetoothGattCharacteristic.PERMISSION_WRITE,
        )

        val outbound = BluetoothGattCharacteristic(
            MeshConstants.OUTBOUND_CHARACTERISTIC_UUID,
            BluetoothGattCharacteristic.PROPERTY_NOTIFY,
            BluetoothGattCharacteristic.PERMISSION_READ,
        )
        val cccd = BluetoothGattDescriptor(
            MeshConstants.CLIENT_CONFIG_DESCRIPTOR_UUID,
            BluetoothGattDescriptor.PERMISSION_READ or BluetoothGattDescriptor.PERMISSION_WRITE,
        )
        outbound.addDescriptor(cccd)
        outboundCharacteristic = outbound

        service.addCharacteristic(inbound)
        service.addCharacteristic(outbound)
        return service
    }
}
