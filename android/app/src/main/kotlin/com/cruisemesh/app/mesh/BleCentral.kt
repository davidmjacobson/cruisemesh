package com.cruisemesh.app.mesh

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.le.BluetoothLeScanner
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.os.ParcelUuid
import android.util.Log
import java.util.ArrayDeque

private const val TAG = "BleCentral"
private const val REQUESTED_MTU = 517

/**
 * Scanner + GATT-client (central) half of the dual BLE role (DESIGN.md §5.2).
 * Scans for the CruiseMesh service UUID, connects, and exchanges frames over
 * the inbound/outbound characteristics. One connection is tracked per
 * discovered device address. A short reconnect cooldown avoids hammering an
 * address that just failed/disconnected (seen in practice against a peer
 * that never completes service discovery); full backoff policy is left for
 * the sync-engine milestone — this is transport plumbing only.
 *
 * Frames larger than one ATT write are fragmented per DESIGN.md §5.2: right
 * after connecting we negotiate the largest MTU the peer allows, then chunk
 * outbound frames with [FrameFraming] (queued and sent one at a time — a
 * GATT connection allows only one in-flight write) and reassemble inbound
 * notifications with a per-peer [FrameReassembler].
 */
@SuppressLint("MissingPermission")
class BleCentral(
    private val context: Context,
    private val onFrameReceived: (ByteArray) -> Unit,
    private val onPeerConnected: (String) -> Unit = {},
) {
    private val bluetoothManager =
        context.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
    private val adapter: BluetoothAdapter? = bluetoothManager.adapter
    private var scanner: BluetoothLeScanner? = null
    private val connections = mutableMapOf<String, BluetoothGatt>()
    private val lastDisconnectAt = mutableMapOf<String, Long>()
    private val negotiatedMtu = mutableMapOf<String, Int>()
    private val reassemblers = mutableMapOf<String, FrameReassembler>()
    private val writeQueues = mutableMapOf<String, ArrayDeque<ByteArray>>()
    private val writeInFlight = mutableSetOf<String>()

    private val scanCallback = object : ScanCallback() {
        override fun onScanResult(callbackType: Int, result: ScanResult) {
            val device = result.device
            if (connections.containsKey(device.address)) return
            val cooldownUntil = (lastDisconnectAt[device.address] ?: 0L) + RECONNECT_COOLDOWN_MS
            if (System.currentTimeMillis() < cooldownUntil) return
            Log.i(TAG, "Discovered peer ${device.address}, connecting")
            connections[device.address] = device.connectGatt(context, false, gattClientCallback)
        }

        override fun onScanFailed(errorCode: Int) {
            Log.w(TAG, "Scan failed: $errorCode")
        }
    }

    private val gattClientCallback = object : BluetoothGattCallback() {
        override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> {
                    // Default (balanced) connection interval is prone to
                    // status=133 on back-to-back GATT ops right after connect.
                    gatt.requestConnectionPriority(BluetoothGatt.CONNECTION_PRIORITY_HIGH)
                    // discoverServices() is deferred to onMtuChanged: like the
                    // descriptor-write/characteristic-write pair below, a
                    // BluetoothGatt allows only one in-flight op at a time.
                    gatt.requestMtu(REQUESTED_MTU)
                }
                BluetoothProfile.STATE_DISCONNECTED -> {
                    Log.i(TAG, "Peer ${gatt.device.address} disconnected (status=$status)")
                    connections.remove(gatt.device.address)
                    lastDisconnectAt[gatt.device.address] = System.currentTimeMillis()
                    negotiatedMtu.remove(gatt.device.address)
                    reassemblers.remove(gatt.device.address)
                    writeQueues.remove(gatt.device.address)
                    writeInFlight.remove(gatt.device.address)
                    gatt.close()
                }
            }
        }

        override fun onMtuChanged(gatt: BluetoothGatt, mtu: Int, status: Int) {
            val effective = if (status == BluetoothGatt.GATT_SUCCESS) mtu else FrameFraming.DEFAULT_ATT_MTU
            Log.i(TAG, "MTU negotiated for ${gatt.device.address}: $effective (status=$status)")
            negotiatedMtu[gatt.device.address] = effective
            gatt.discoverServices()
        }

        override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
            val service = gatt.getService(MeshConstants.SERVICE_UUID) ?: return
            val outbound = service.getCharacteristic(MeshConstants.OUTBOUND_CHARACTERISTIC_UUID) ?: return
            gatt.setCharacteristicNotification(outbound, true)
            val cccd = outbound.getDescriptor(MeshConstants.CLIENT_CONFIG_DESCRIPTOR_UUID) ?: return
            cccd.value = BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
            gatt.writeDescriptor(cccd)
            Log.i(TAG, "Services discovered for ${gatt.device.address}; subscribing to outbound")
            // Do not touch this gatt again until onDescriptorWrite confirms the
            // subscription: BluetoothGatt allows only one in-flight operation at
            // a time and silently rejects (returns false) anything issued sooner.
        }

        @Deprecated("Deprecated in Android API 33+; minSdk 26 still needs this overload")
        override fun onCharacteristicChanged(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
        ) {
            if (characteristic.uuid != MeshConstants.OUTBOUND_CHARACTERISTIC_UUID) return
            val reassembler = reassemblers.getOrPut(gatt.device.address) { FrameReassembler() }
            reassembler.accept(characteristic.value)?.let(onFrameReceived)
        }

        override fun onDescriptorWrite(
            gatt: BluetoothGatt,
            descriptor: BluetoothGattDescriptor,
            status: Int,
        ) {
            Log.i(TAG, "Descriptor write for ${gatt.device.address} status=$status")
            if (status == BluetoothGatt.GATT_SUCCESS) {
                onPeerConnected(gatt.device.address)
            }
        }

        @Deprecated("Deprecated in Android API 33+; minSdk 26 still needs this overload")
        override fun onCharacteristicWrite(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
            status: Int,
        ) {
            Log.i(TAG, "Characteristic write for ${gatt.device.address} status=$status")
            writeInFlight.remove(gatt.device.address)
            sendNextQueuedFragment(gatt)
        }
    }

    fun start() {
        val btAdapter = adapter ?: run {
            Log.w(TAG, "No Bluetooth adapter; cannot start central role")
            return
        }
        scanner = btAdapter.bluetoothLeScanner
        val filter = ScanFilter.Builder()
            .setServiceUuid(ParcelUuid(MeshConstants.SERVICE_UUID))
            .build()
        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_BALANCED)
            .build()
        scanner?.startScan(listOf(filter), settings, scanCallback)
    }

    fun stop() {
        scanner?.stopScan(scanCallback)
        connections.values.forEach { it.close() }
        connections.clear()
        negotiatedMtu.clear()
        reassemblers.clear()
        writeQueues.clear()
        writeInFlight.clear()
    }

    /**
     * Write a frame to a specific connected peer's inbound characteristic,
     * fragmenting per the peer's negotiated MTU if needed (DESIGN.md §5.2).
     */
    fun sendFrame(deviceAddress: String, frame: ByteArray) {
        val gatt = connections[deviceAddress] ?: run {
            Log.w(TAG, "sendFrame: no connection tracked for $deviceAddress")
            return
        }
        val payloadSize = (negotiatedMtu[deviceAddress] ?: FrameFraming.DEFAULT_ATT_MTU) -
            FrameFraming.ATT_HEADER_OVERHEAD
        val fragments = FrameFraming.fragment(frame, payloadSize)
        writeQueues.getOrPut(deviceAddress) { ArrayDeque() }.addAll(fragments)
        Log.i(TAG, "sendFrame: queued ${fragments.size} fragment(s) for $deviceAddress (${frame.size} bytes)")
        sendNextQueuedFragment(gatt)
    }

    private fun sendNextQueuedFragment(gatt: BluetoothGatt) {
        val address = gatt.device.address
        if (address in writeInFlight) return
        val fragment = writeQueues[address]?.poll() ?: return
        val service = gatt.getService(MeshConstants.SERVICE_UUID) ?: run {
            Log.w(TAG, "sendNextQueuedFragment: service not found on $address")
            return
        }
        val inbound = service.getCharacteristic(MeshConstants.INBOUND_CHARACTERISTIC_UUID) ?: run {
            Log.w(TAG, "sendNextQueuedFragment: inbound characteristic not found on $address")
            return
        }
        inbound.value = fragment
        if (gatt.writeCharacteristic(inbound)) {
            writeInFlight += address
        } else {
            Log.w(TAG, "sendNextQueuedFragment: writeCharacteristic rejected for $address")
        }
    }

    companion object {
        private const val RECONNECT_COOLDOWN_MS = 5_000L
    }
}
