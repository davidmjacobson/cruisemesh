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

    private val advertiseCallback = object : AdvertiseCallback() {
        override fun onStartSuccess(settingsInEffect: AdvertiseSettings) {
            Log.i(TAG, "Advertising started")
        }

        override fun onStartFailure(errorCode: Int) {
            Log.w(TAG, "Advertising failed: $errorCode")
        }
    }

    private val gattServerCallback = object : BluetoothGattServerCallback() {
        override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
            Log.i(TAG, "Central ${device.address} connection state=$newState")
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> connectedDevices[device.address] = device
                BluetoothProfile.STATE_DISCONNECTED -> {
                    connectedDevices.remove(device.address)
                    negotiatedMtu.remove(device.address)
                    reassemblers.remove(device.address)
                    notifyQueues.remove(device.address)
                    notifyInFlight.remove(device.address)
                    onCentralDisconnected(device.address)
                }
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
        advertiser?.startAdvertising(settings, data, scanResponse, advertiseCallback)
    }

    fun stop() {
        // stopAdvertising throws if the adapter is already off -- the case when
        // stop() runs because Bluetooth was turned off. Swallow it; advertising
        // is gone either way.
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
            Log.w(TAG, "sendNextQueuedFragment: notifyCharacteristicChanged rejected for $address")
        }
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
