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
import android.os.ParcelUuid
import android.util.Log
import java.util.ArrayDeque

private const val TAG = "BlePeripheral"

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
    private val notifyQueues = mutableMapOf<String, ArrayDeque<ByteArray>>()
    private val notifyInFlight = mutableSetOf<String>()
    @Volatile private var advertising = false

    private val advertiseCallback = object : AdvertiseCallback() {
        override fun onStartSuccess(settingsInEffect: AdvertiseSettings) {
            advertising = true
            Log.i(TAG, "Advertising started with txPower=${settingsInEffect.txPowerLevel}")
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
            if (status != BluetoothGatt.GATT_SUCCESS) {
                // Mirrors BleCentral's onCharacteristicWrite hardening: a
                // rejected notify means this central's link is already dead
                // on its side (supervision-timeout death, stale gatt, etc.)
                // even though our GATT server object still thinks it's
                // connected -- the exact "phone A thinks the link is alive,
                // phone B saw it die 30s ago" blackhole observed live
                // 2026-07-10. cancelConnection() plus the map cleanup below
                // returns this address to the digest-sync retry path instead
                // of pretending the send succeeded.
                Log.w(
                    TAG,
                    "onNotificationSent: notify failed for ${device.address} (status=$status); tearing down link",
                )
                gattServer?.cancelConnection(device)
                tearDownLink(device.address, "notification send failed (status=$status)")
                return
            }
            notifyInFlight.remove(device.address)
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
            // TX power is HIGH for the range experiment; this changes discovery
            // reach without increasing the BALANCED advertising duty cycle.
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_BALANCED)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_HIGH)
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
        connectedDevices.clear()
        negotiatedMtu.clear()
        reassemblers.clear()
        notifyQueues.clear()
        notifyInFlight.clear()
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
            notifyQueues.getOrPut(device.address) { ArrayDeque() }.addAll(fragments)
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
        notifyQueues.getOrPut(deviceAddress) { ArrayDeque() }.addAll(fragments)
        Log.i(TAG, "sendFrame: queued ${fragments.size} fragment(s) for $deviceAddress (${frame.size} bytes)")
        sendNextQueuedFragment(device)
    }

    private fun sendNextQueuedFragment(device: BluetoothDevice) {
        val address = device.address
        if (address in notifyInFlight) return
        val fragment = notifyQueues[address]?.poll() ?: return
        val characteristic = outboundCharacteristic ?: return
        val server = gattServer ?: return
        characteristic.value = fragment
        // notifyCharacteristicChanged throws IllegalArgumentException on an
        // oversized value; letting that unwind here would abandon the GATT
        // callback it runs on (e.g. a central never gets its write response and
        // the link times out). FrameFraming keeps fragments within the ATT cap,
        // so this guard is belt-and-suspenders against any future bad fragment.
        val notified = try {
            server.notifyCharacteristicChanged(device, characteristic, false)
        } catch (e: Exception) {
            Log.w(TAG, "sendNextQueuedFragment: notify threw for $address (${e.message}); dropping fragment")
            false
        }
        if (notified) {
            notifyInFlight += address
        } else {
            // Same reasoning as onNotificationSent's failure branch: a
            // rejected notify on a link we believe is established (the
            // in-flight guard above only lets one notify through at a time)
            // means the link isn't usable. Leaving it mapped would
            // black-hole every future send to this address.
            Log.w(TAG, "sendNextQueuedFragment: notifyCharacteristicChanged rejected for $address; tearing down link")
            gattServer?.cancelConnection(device)
            tearDownLink(address, "notifyCharacteristicChanged rejected")
        }
    }

    /**
     * Single per-address teardown path shared by the normal
     * STATE_DISCONNECTED callback and the notify-failure paths
     * (onNotificationSent / sendNextQueuedFragment) so the map cleanup and
     * the [onCentralDisconnected] signal to MeshRouter can never drift apart
     * -- mirrors [BleCentral.tearDownLink] (see that class's doc comment for
     * the blackhole bug this fixes).
     */
    private fun tearDownLink(address: String, reason: String) {
        Log.i(TAG, "tearDownLink: $address ($reason)")
        connectedDevices.remove(address)
        negotiatedMtu.remove(address)
        reassemblers.remove(address)
        notifyQueues.remove(address)
        notifyInFlight.remove(address)
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
