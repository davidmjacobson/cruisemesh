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
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import android.util.Log

private const val TAG = "BleCentral"
private const val REQUESTED_MTU = 517

// Live capture 2026-07-10 (two phones side by side in a car): a connectGatt
// at 20:18:02.645 produced no callback at all until status=147 at
// 20:18:32.662 -- a hung connect held one of Android's ~7 GATT client slots
// for a full 30 seconds. With the dual-role mesh using 2 links per peer
// pair, one unreachable (stale/rotated) address starves the slot pool and
// every other peer churns status=133 waiting for a free slot. The connect
// setup burst (connect -> MTU -> discover -> subscribe) takes ~300 ms in
// practice at CONNECTION_PRIORITY_HIGH, so 12 s is generous headroom while
// still freeing the slot well before the stack's own ~30 s give-up.
private const val CONNECT_TIMEOUT_MS = 12_000L

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
 *
 * Link-death hardening (2026-07-10 silent-blackhole bug, see the log
 * evidence above [CONNECT_TIMEOUT_MS]): a hung connectGatt now gets a
 * watchdog that frees the GATT slot instead of squatting on it for ~30 s
 * (see [connectWatchdogs]/[fullyConnected]), and a failed write is now
 * treated as proof the link is dead -- [tearDownLink] runs so MeshRouter
 * unmaps the address instead of continuing to report `sendToUserId` success
 * into the void. Frames already queued for a torn-down address are not
 * lost: they live in the persistent store and redeliver via digest sync the
 * next time that peer (or its readvertised address) reconnects.
 *
 * Thread-safety hardening (FA2, 2026-07-20): every map above is per-address
 * shared state mutated from GATT binder threads (each callback below), the
 * main thread ([scanCallback], the connect watchdog), and any caller thread
 * via [sendFrame] (MeshRouter dispatches into this class from peripheral
 * binder threads, LAN reader threads, and the relay-sync thread). This is
 * the exact race [BlePeripheral] was already hardened against (see its
 * `lock` doc comment) -- BleCentral never got the same fix, so e.g. the old
 * check-then-act on the write-in-flight set could let two threads both
 * observe "not in flight" and both issue a GATT write for the same address
 * at once. [lock] now guards every read-modify-write of the per-address maps
 * (mirrors [BlePeripheral]'s single-lock design); the write-queue/in-flight
 * admission decision itself is extracted into [GattWriteQueue], a plain
 * Android-import-free class unit-tested directly (TODO.md §3.4 pattern, same
 * as [NotifyFailureTracker]/[ReconnectBackoffTracker]). GATT/binder calls
 * (`connectGatt`, `writeCharacteristic`, `gatt.close()`, ...) are kept
 * outside [lock] where possible -- decisions are computed under the lock,
 * the framework call happens after -- so this class never holds a lock
 * across a binder call.
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
    private val writeQueue = GattWriteQueue()
    private val scanDiagnostics = mutableMapOf<String, ScanDiagnostics>()

    // Guards every read-modify-write of the per-address state above
    // (connections, negotiatedMtu, reassemblers, scanDiagnostics,
    // fullyConnected, connectWatchdogs) -- see the FA2 hardening note in this
    // class's doc comment. [GattWriteQueue] is its own leaf-synchronized
    // class (mirrors [NotifyFailureTracker]), so it is safe to call with or
    // without this lock held; call sites below take it anyway for a single
    // consistent locking story. Lock ordering: only leaf locks
    // ([GattWriteQueue]'s, [ReconnectBackoffTracker]'s underlying core
    // mutex) are ever taken while holding this one, so callouts made after
    // releasing it (onPeerConnected/onPeerDisconnected/onFrameReceived, all
    // called outside the lock below) cannot deadlock against it.
    private val lock = Any()

    // GATT callbacks arrive on a binder thread, but Handler.postDelayed's
    // callback runs on whichever Looper the Handler was built with -- use
    // the main looper so watchdog firing (and its map mutations) lands on
    // the same thread as the rest of this class's Android-framework calls.
    private val handler = Handler(Looper.getMainLooper())

    // Per-address pending "connect is taking too long" timer, keyed the same
    // way as [connections] so a watchdog can always be found and cancelled
    // by address alone. Guarded by [lock].
    private val connectWatchdogs = mutableMapOf<String, Runnable>()

    // Addresses that have completed the full connect -> MTU -> discover ->
    // subscribe setup burst (i.e. onPeerConnected has fired for them). The
    // watchdog only acts on an address that is tracked in [connections] but
    // missing from this set -- once setup completes, the connection is
    // healthy and the watchdog is cancelled (see onDescriptorWrite) rather
    // than left to fire uselessly. Guarded by [lock].
    private val fullyConnected = mutableSetOf<String>()

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
            synchronized(lock) { scanDiagnostics[device.address] = diagnostics }

            if (ownInstanceData != null && ownInstanceData.contentEquals(MeshConstants.LOCAL_INSTANCE_ID)) {
                Log.i(TAG, "Ignoring own advertisement from ${device.address} ($diagnostics)")
                return
            }

            if (synchronized(lock) { connections.containsKey(device.address) }) return
            val now = System.currentTimeMillis()
            if (!backoff.canAttempt(device.address, now)) return
            Log.i(TAG, "Discovered peer ${device.address}, connecting ($diagnostics)")
            val gatt = device.connectGatt(context, false, gattClientCallback)
            synchronized(lock) {
                connections[device.address] = gatt
                scheduleConnectWatchdogLocked(device.address, gatt)
            }
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
                    val diagnostics = synchronized(lock) { scanDiagnostics[address] }
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
                    tearDownLink(gatt, "disconnected (status=$status)")
                }
            }
        }

        override fun onMtuChanged(gatt: BluetoothGatt, mtu: Int, status: Int) {
            val effective = if (status == BluetoothGatt.GATT_SUCCESS) mtu else FrameFraming.DEFAULT_ATT_MTU
            Log.i(TAG, "MTU negotiated for ${gatt.device.address}: $effective (status=$status)")
            synchronized(lock) { negotiatedMtu[gatt.device.address] = effective }
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
            // Only the map access needs the lock; reassembly and frame
            // handling stay outside it so inbound processing never
            // serializes against the send paths (mirrors BlePeripheral's
            // onCharacteristicWriteRequest).
            val reassembler = synchronized(lock) {
                reassemblers.getOrPut(address) { FrameReassembler() }
            }
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
                val address = gatt.device.address
                // Fully connected: clear this address's failure history so a
                // peer that connects reliably never accumulates backoff.
                backoff.recordSuccess(address)
                // Setup is done -- the connect watchdog (1a) no longer has
                // anything to guard against, and must not fire and tear
                // down a perfectly healthy link.
                synchronized(lock) {
                    fullyConnected += address
                    cancelConnectWatchdogLocked(address)
                }
                onPeerConnected(address)
            }
        }

        @Deprecated("Deprecated in Android API 33+; minSdk 26 still needs this overload")
        override fun onCharacteristicWrite(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
            status: Int,
        ) {
            Log.i(TAG, "Characteristic write for ${gatt.device.address} status=$status")
            writeQueue.completeWrite(gatt.device.address)
            if (status != BluetoothGatt.GATT_SUCCESS) {
                // A failed write on a link this class still believes is
                // live is exactly the observed blackhole: the far side's
                // supervision-timeout death (status=147) is invisible here
                // except as a rejected write, and the old code just logged
                // and moved on, leaving the address mapped in MeshRouter
                // forever ("sendToUserId" kept returning true while frames
                // evaporated). Tear the link down so the address gets
                // unmapped -- the fragment we just failed to deliver is not
                // lost, it lives in the persistent store and redelivers via
                // digest sync the next time this peer (or its readvertised
                // address) reconnects.
                Log.w(
                    TAG,
                    "onCharacteristicWrite: write failed for ${gatt.device.address} " +
                        "(status=$status); tearing down link",
                )
                backoff.recordFailure(gatt.device.address, System.currentTimeMillis())
                tearDownLink(gatt, "characteristic write failed (status=$status)")
                return
            }
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
            // BALANCED (restored from LOW_POWER 2026-07-10): the LOW_POWER scan
            // duty cycle was too small for the central to catch a peer's
            // advertising window, so direct connects churned with status=133 and
            // the initial connect was flaky/slow. Since the mesh no longer pauses
            // for Bluetooth audio, connection reliability wins here -- re-verify
            // earbud audio doesn't stutter at this higher scan duty. (Result
            // batching via setReportDelay would help further but routes results
            // to onBatchScanResults, which this callback doesn't implement.)
            .setScanMode(ScanSettings.SCAN_MODE_BALANCED)
            .build()
        scanner?.startScan(listOf(filter), settings, scanCallback)
    }

    fun stop() {
        // stopScan throws IllegalStateException if the adapter is already off --
        // which is exactly the case when stop() runs in response to Bluetooth
        // being turned off. Swallow it; the scan is gone either way.
        try {
            scanner?.stopScan(scanCallback)
        } catch (e: Exception) {
            Log.w(TAG, "stopScan during stop() failed (adapter likely off): ${e.message}")
        }
        scanner = null
        // gatt.close() is a binder call -- snapshot the connections under the
        // lock, then close them after releasing it, mirroring BlePeripheral's
        // notifyFrame() pattern of taking the lock only to copy state out.
        val connectionsSnapshot = synchronized(lock) {
            connectWatchdogs.values.forEach { handler.removeCallbacks(it) }
            connectWatchdogs.clear()
            fullyConnected.clear()
            val snapshot = connections.values.toList()
            connections.clear()
            negotiatedMtu.clear()
            reassemblers.clear()
            scanDiagnostics.clear()
            snapshot
        }
        connectionsSnapshot.forEach { runCatching { it.close() } }
        writeQueue.clearAll()
    }

    /**
     * Write a frame to a specific connected peer's inbound characteristic,
     * fragmenting per the peer's negotiated MTU if needed (DESIGN.md §5.2).
     */
    fun sendFrame(deviceAddress: String, frame: ByteArray) {
        val gatt = synchronized(lock) { connections[deviceAddress] } ?: run {
            Log.w(TAG, "sendFrame: no connection tracked for $deviceAddress")
            return
        }
        val mtu = synchronized(lock) { negotiatedMtu[deviceAddress] } ?: FrameFraming.DEFAULT_ATT_MTU
        val payloadSize = mtu - FrameFraming.ATT_HEADER_OVERHEAD
        val fragments = FrameFraming.fragmentOrNull(frame, payloadSize) ?: run {
            Log.w(TAG, "sendFrame: dropping ${frame.size}-byte frame for $deviceAddress -- too large to fragment")
            return
        }
        writeQueue.enqueue(deviceAddress, fragments)
        Log.i(TAG, "sendFrame: queued ${fragments.size} fragment(s) for $deviceAddress (${frame.size} bytes)")
        sendNextQueuedFragment(gatt)
    }

    /**
     * Admits and writes this address's next queued fragment, unless one is
     * already in flight (a GATT connection allows only one outstanding
     * characteristic write at a time). [GattWriteQueue.admitNext] does the
     * in-flight check, the FIFO pop, and the slot reservation atomically --
     * see that class's doc comment for the race this closes. The reservation
     * happens before the GATT call below, so a concurrent caller for the
     * same address (another thread racing in via [sendFrame], or the async
     * onCharacteristicWrite ack) can never observe the slot as free while a
     * write is actually in flight, without this class having to hold [lock]
     * across the binder call itself.
     */
    private fun sendNextQueuedFragment(gatt: BluetoothGatt) {
        val address = gatt.device.address
        val fragment = writeQueue.admitNext(address) ?: return
        val service = gatt.getService(MeshConstants.SERVICE_UUID) ?: run {
            Log.w(TAG, "sendNextQueuedFragment: service not found on $address")
            writeQueue.completeWrite(address)
            return
        }
        val inbound = service.getCharacteristic(MeshConstants.INBOUND_CHARACTERISTIC_UUID) ?: run {
            Log.w(TAG, "sendNextQueuedFragment: inbound characteristic not found on $address")
            writeQueue.completeWrite(address)
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
        if (!written) {
            // Same reasoning as the onCharacteristicWrite failure branch: a
            // rejected write on a link we believe is established (the
            // in-flight guard above only lets one write through at a time,
            // so this is not a "too busy" rejection) means the link isn't
            // usable -- dead peer or stale gatt. Leaving it mapped would
            // black-hole every future send to this address. tearDownLink
            // clears the write queue's reservation for us; on success the
            // slot stays reserved until the async onCharacteristicWrite ack
            // calls writeQueue.completeWrite.
            Log.w(TAG, "sendNextQueuedFragment: writeCharacteristic rejected for $address; tearing down link")
            backoff.recordFailure(address, System.currentTimeMillis())
            tearDownLink(gatt, "writeCharacteristic rejected")
        }
    }

    /**
     * Schedules the connect watchdog (see [CONNECT_TIMEOUT_MS]) for a
     * freshly-issued connectGatt. If [address] has not reached
     * [fullyConnected] by the time this fires, the connect is presumed
     * hung -- the Android BLE stack was observed live holding a client slot
     * for ~30 s with no callback at all before finally delivering
     * status=147, and with only ~7 client slots shared across the whole
     * dual-role mesh, one hung connect starves every other peer. Freeing
     * the slot at 12 s (instead of waiting out the stack's own timeout)
     * keeps the slot pool available for peers that can actually connect.
     *
     * Callers must hold [lock] (schedules [connectWatchdogs]); the watchdog
     * [Runnable] itself fires later off the main looper and takes [lock]
     * itself when it does, since by then the scheduling call has long since
     * returned.
     */
    private fun scheduleConnectWatchdogLocked(address: String, gatt: BluetoothGatt) {
        val watchdog = Runnable {
            val hung = synchronized(lock) {
                !(address in fullyConnected || connections[address] !== gatt)
            }
            if (!hung) {
                // Already completed setup, or superseded by a newer
                // connection attempt for this address -- nothing to do.
                return@Runnable
            }
            Log.w(TAG, "connect watchdog: $address stuck for ${CONNECT_TIMEOUT_MS}ms; freeing slot")
            runCatching { gatt.disconnect() }
            runCatching { gatt.close() }
            tearDownLink(gatt, "connect watchdog timeout (${CONNECT_TIMEOUT_MS}ms)")
            backoff.recordFailure(address, System.currentTimeMillis())
        }
        connectWatchdogs[address] = watchdog
        handler.postDelayed(watchdog, CONNECT_TIMEOUT_MS)
    }

    /** Callers must hold [lock]; see [scheduleConnectWatchdogLocked]. */
    private fun cancelConnectWatchdogLocked(address: String) {
        connectWatchdogs.remove(address)?.let { handler.removeCallbacks(it) }
    }

    /**
     * Single per-address teardown path shared by the normal
     * STATE_DISCONNECTED callback, the connect watchdog, and the
     * send-failure paths (onCharacteristicWrite / sendNextQueuedFragment) so
     * none of them can drift out of sync with each other -- a link that's
     * cleared from these maps but never signalled to MeshRouter (or vice
     * versa) is exactly the silent-blackhole bug this class is hardened
     * against. Always safe to call more than once for the same gatt
     * (BluetoothGatt#close is idempotent); callers that already know the
     * disconnect reason (status, failure count, diagnostics) log that
     * themselves before calling in, so [reason] only needs to identify
     * *which* call site triggered the teardown.
     *
     * Takes [lock] itself for the map cleanup, then calls out to
     * [onPeerDisconnected] and `gatt.close()` (a binder call) after
     * releasing it -- neither needs to run while other threads are blocked
     * out of the maps, and this keeps a binder call from ever happening
     * while [lock] is held.
     */
    private fun tearDownLink(gatt: BluetoothGatt, reason: String) {
        val address = gatt.device.address
        Log.i(TAG, "tearDownLink: $address ($reason)")
        synchronized(lock) {
            connections.remove(address)
            negotiatedMtu.remove(address)
            reassemblers.remove(address)
            fullyConnected.remove(address)
            cancelConnectWatchdogLocked(address)
        }
        writeQueue.clear(address)
        onPeerDisconnected(address)
        gatt.close()
    }
}
