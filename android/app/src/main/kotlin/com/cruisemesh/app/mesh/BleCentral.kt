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
 * discovered device address.
 *
 * Connection churn hardening (2026-07-08 status=133 retry-loop bug): a
 * per-address [ReconnectBackoffTracker] replaces the old flat cooldown --
 * repeated failures to the same address back off exponentially and, past a
 * failure budget, that address is given up on entirely (it's presumably a
 * stale/rotated BLE address; a real peer re-advertising is rediscovered
 * under a fresh address with no prior failure history). The tracker resets
 * for an address the moment it fully connects. Separately, since a device
 * can't reliably read its own BLE address (rotates, and
 * BluetoothAdapter#getAddress is a dummy constant since API 23) to filter
 * out its own advertisement, [MeshConstants.LOCAL_INSTANCE_ID] is compared
 * against each scan result's service data as a self-connection guard.
 *
 * Frames larger than one ATT write are fragmented per DESIGN.md §5.2: right
 * after connecting we negotiate the largest MTU the peer allows, then chunk
 * outbound frames with [FrameFraming] (queued and sent one at a time — a
 * GATT connection allows only one in-flight write) and reassemble inbound
 * notifications with a per-peer [FrameReassembler].
 *
 * Milestone 1 (DESIGN.md §5.2, §7.3): callers need to know *which* peer a
 * frame came from to route replies and receipts, so [onFrameReceived] now
 * carries the device address alongside the frame bytes. [onPeerConnected]
 * fires once this link can carry frames (see the existing descriptor-write
 * comment below); [onPeerDisconnected] fires so callers (MeshRouter, via
 * MeshService) can drop a stale address mapping instead of sending into the
 * void.
 */
@SuppressLint("MissingPermission")
class BleCentral(
    private val context: Context,
    private val onFrameReceived: (address: String, frame: ByteArray) -> Unit,
    private val onPeerConnected: (String) -> Unit = {},
    private val onPeerDisconnected: (String) -> Unit = {},
) {
    private val bluetoothManager =
        context.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
    private val adapter: BluetoothAdapter? = bluetoothManager.adapter
    private var scanner: BluetoothLeScanner? = null
    private val connections = mutableMapOf<String, BluetoothGatt>()
    private val backoff = ReconnectBackoffTracker()
    private val negotiatedMtu = mutableMapOf<String, Int>()
    private val reassemblers = mutableMapOf<String, FrameReassembler>()
    private val writeQueues = mutableMapOf<String, ArrayDeque<ByteArray>>()
    private val writeInFlight = mutableSetOf<String>()
    private val scanDiagnostics = mutableMapOf<String, ScanDiagnostics>()

    /**
     * Advertised service UUIDs + device name + whether our service *data*
     * (the scan-response payload the self-connection guard reads, distinct
     * from the advertised UUID list) was present, captured at discovery
     * time for logging. `hasServiceData` exists specifically to settle the
     * open question in known-issue #2 of HANDOFF.md's churn notes: is a
     * churning address missing scan-response service data (a guard blind
     * spot) or is it a genuinely stale rotated peer address? If churn logs
     * ever show `hasServiceData=false` for a churning peer that still
     * advertises our service UUID, that's the guard blind spot; if it's
     * always `true`, the guard is seeing the data and rejecting for some
     * other reason, and rotated stale addresses are the more likely cause.
     */
    private data class ScanDiagnostics(
        val serviceUuids: List<String>,
        val deviceName: String?,
        val hasServiceData: Boolean,
    ) {
        override fun toString() = "serviceUuids=$serviceUuids name=${deviceName ?: "?"} hasServiceData=$hasServiceData"
    }

    private val scanCallback = object : ScanCallback() {
        override fun onScanResult(callbackType: Int, result: ScanResult) {
            val device = result.device
            val record = result.scanRecord
            val ownInstanceData = record?.getServiceData(ParcelUuid(MeshConstants.SERVICE_UUID))
            val diagnostics = ScanDiagnostics(
                serviceUuids = record?.serviceUuids?.map { it.toString() } ?: emptyList(),
                deviceName = record?.deviceName,
                hasServiceData = ownInstanceData != null,
            )
            scanDiagnostics[device.address] = diagnostics

            if (ownInstanceData != null && ownInstanceData.contentEquals(MeshConstants.LOCAL_INSTANCE_ID)) {
                Log.i(TAG, "Ignoring own advertisement from ${device.address} ($diagnostics)")
                return
            }

            if (connections.containsKey(device.address)) return
            val now = System.currentTimeMillis()
            if (!backoff.canAttempt(device.address, now)) return
            Log.i(TAG, "Discovered peer ${device.address}, connecting ($diagnostics)")
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
                    // HIGH (minimum ~7.5 ms connection interval) *only* for the
                    // connect/MTU/discover/CCCD setup burst, which is prone to
                    // status=133 on back-to-back GATT ops at the balanced
                    // interval. It is relaxed back to BALANCED in
                    // onDescriptorWrite once the link is fully set up -- holding
                    // 7.5 ms at rest pegs the shared 2.4 GHz radio and starves
                    // the phone's own Bluetooth (A2DP) audio (HANDOFF: BLE
                    // coexistence blocker). The mesh only ships occasional small
                    // text frames, so it does not need a fast link at rest.
                    gatt.requestConnectionPriority(BluetoothGatt.CONNECTION_PRIORITY_HIGH)
                    // discoverServices() is deferred to onMtuChanged: like the
                    // descriptor-write/characteristic-write pair below, a
                    // BluetoothGatt allows only one in-flight op at a time.
                    gatt.requestMtu(REQUESTED_MTU)
                }
                BluetoothProfile.STATE_DISCONNECTED -> {
                    val address = gatt.device.address
                    val diagnostics = scanDiagnostics[address]
                    val failures = backoff.recordFailure(address, System.currentTimeMillis())
                    Log.i(
                        TAG,
                        "Peer $address disconnected (status=$status, consecutive failures=$failures) ($diagnostics)",
                    )
                    if (backoff.isGivenUp(address)) {
                        Log.w(
                            TAG,
                            "Giving up on $address after $failures consecutive failures " +
                                "(likely stale/rotated address); will retry only if rediscovered " +
                                "under a fresh advertisement address ($diagnostics)",
                        )
                    }
                    connections.remove(address)
                    negotiatedMtu.remove(address)
                    reassemblers.remove(address)
                    writeQueues.remove(address)
                    writeInFlight.remove(address)
                    onPeerDisconnected(address)
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
            val address = gatt.device.address
            val reassembler = reassemblers.getOrPut(address) { FrameReassembler() }
            reassembler.accept(characteristic.value)?.let { onFrameReceived(address, it) }
        }

        override fun onDescriptorWrite(
            gatt: BluetoothGatt,
            descriptor: BluetoothGattDescriptor,
            status: Int,
        ) {
            Log.i(TAG, "Descriptor write for ${gatt.device.address} status=$status")
            if (status == BluetoothGatt.GATT_SUCCESS) {
                // Setup burst is done -- drop the connection interval back from
                // HIGH (~7.5 ms) to BALANCED so an idle mesh link stops hogging
                // the shared radio and killing Bluetooth audio (HANDOFF: BLE
                // coexistence blocker). Frames still flow fine at the balanced
                // interval; this only trades a little added latency on the
                // occasional text frame for the phone's audio staying usable.
                gatt.requestConnectionPriority(BluetoothGatt.CONNECTION_PRIORITY_BALANCED)
                // Fully connected: clear this address's failure history so a
                // peer that connects reliably never accumulates backoff.
                backoff.recordSuccess(gatt.device.address)
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
        if (scanner != null) {
            // Idempotent for the same reason as BlePeripheral.start(): a
            // second start must not disturb live connections (the duplicate
            // startScan alone just fails with SCAN_FAILED_ALREADY_STARTED,
            // but keeping the guard symmetric makes the contract obvious).
            Log.i(TAG, "start: central role already running; ignoring")
            return
        }
        scanner = btAdapter.bluetoothLeScanner
        val filter = ScanFilter.Builder()
            .setServiceUuid(ParcelUuid(MeshConstants.SERVICE_UUID))
            .build()
        val settings = ScanSettings.Builder()
            // LOW_POWER (was BALANCED): a much smaller scan duty cycle so
            // continuous discovery stops monopolizing the shared 2.4 GHz radio
            // and starving Bluetooth audio (HANDOFF: BLE coexistence blocker).
            // Discovery is a little slower to notice a new peer; acceptable for
            // a mesh that stays connected once peers are found. (Result
            // batching via setReportDelay would help further but routes results
            // to onBatchScanResults, which this callback doesn't implement.)
            .setScanMode(ScanSettings.SCAN_MODE_LOW_POWER)
            .build()
        scanner?.startScan(listOf(filter), settings, scanCallback)
    }

    fun stop() {
        scanner?.stopScan(scanCallback)
        scanner = null
        connections.values.forEach { it.close() }
        connections.clear()
        negotiatedMtu.clear()
        reassemblers.clear()
        writeQueues.clear()
        writeInFlight.clear()
        scanDiagnostics.clear()
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
        // writeCharacteristic can throw on an oversized value; keep it from
        // unwinding this GATT callback (mirrors BlePeripheral). FrameFraming
        // already caps fragments at the ATT limit, so this is a safety net.
        val written = try {
            gatt.writeCharacteristic(inbound)
        } catch (e: Exception) {
            Log.w(TAG, "sendNextQueuedFragment: write threw for $address (${e.message}); dropping fragment")
            false
        }
        if (written) {
            writeInFlight += address
        } else {
            Log.w(TAG, "sendNextQueuedFragment: writeCharacteristic rejected for $address")
        }
    }

}
