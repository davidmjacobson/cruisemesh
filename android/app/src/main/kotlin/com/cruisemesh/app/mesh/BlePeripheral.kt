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
 */
@SuppressLint("MissingPermission")
class BlePeripheral(
    private val context: Context,
    private val onFrameReceived: (ByteArray) -> Unit,
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
                reassembler.accept(value)?.let(onFrameReceived)
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

        gattServer = bluetoothManager.openGattServer(context, gattServerCallback)?.also { server ->
            server.addService(buildGattService())
        }

        advertiser = btAdapter.bluetoothLeAdvertiser
        val settings = AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_BALANCED)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
            .setConnectable(true)
            .build()
        val data = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .addServiceUuid(ParcelUuid(MeshConstants.SERVICE_UUID))
            .build()
        advertiser?.startAdvertising(settings, data, advertiseCallback)
    }

    fun stop() {
        advertiser?.stopAdvertising(advertiseCallback)
        gattServer?.close()
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
            val fragments = FrameFraming.fragment(frame, payloadSize)
            notifyQueues.getOrPut(device.address) { ArrayDeque() }.addAll(fragments)
            sendNextQueuedFragment(device)
        }
    }

    private fun sendNextQueuedFragment(device: BluetoothDevice) {
        val address = device.address
        if (address in notifyInFlight) return
        val fragment = notifyQueues[address]?.poll() ?: return
        val characteristic = outboundCharacteristic ?: return
        val server = gattServer ?: return
        characteristic.value = fragment
        if (server.notifyCharacteristicChanged(device, characteristic, false)) {
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
