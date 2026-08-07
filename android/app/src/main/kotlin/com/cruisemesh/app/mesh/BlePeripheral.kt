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
import android.os.SystemClock
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

// Inbound half of the ACL-slot budget (see PeripheralLinkAdmission for the
// full reasoning). BleCentral's MAX_CENTRAL_LINKS rations the 5 links this
// phone chooses to open; until now nothing rationed the links other phones
// opened *to* us, even though both come out of the same ~7-8 concurrent
// connections the controller offers. 5 + 3 is that ceiling, so the two roles
// now describe one budget instead of one role budgeting against an unbounded
// other. Three inbound links is redundancy, not reach: MeshRouter elects a
// single route per logical peer, so the inbound half of a dual-role pair
// matters only when our own central cannot get a slot to that peer -- which
// is exactly the case a spare inbound slot serves. A typical family sits far
// below this; it only bites in a dense fleet.
private const val MAX_PERIPHERAL_LINKS = 3

// A central turned away at the cap reconnects on its next scan hit, so in a
// saturated room the rejection path runs constantly -- throttle its log so it
// reports the condition without reproducing the very spam it prevents
// (mirrors BleCentral's AT_CAP_LOG_INTERVAL_MS).
private const val AT_CAP_LOG_INTERVAL_MS = 10_000L

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
 *
 * Adaptive advertise duty (battery, 2026-07-21): [setAdvertiseDutyMode] lets
 * [MeshService] drive [AdvertiseSettings]' mode from the same
 * [RadioPowerPolicy] decision [BleCentral] uses for scanning -- LOW_POWER
 * once at least one link is up and the quiet period holds, BALANCED while
 * lonely or right after a link change. TX power stays MEDIUM regardless
 * (range, not coexistence). Advertising restarts on every central connect
 * (legacy advertising auto-stops on connect -- PR#17) and after every
 * teardown; [setAdvertiseDutyMode] reuses that exact restart path so a mode
 * change while already advertising doesn't need a second one.
 *
 * Advertising restart (2026-08-07): every one of those restarts used to reuse
 * a single [AdvertiseCallback] instance, which is the object
 * [BluetoothLeAdvertiser] keys its own wrapper map on -- so the restart was a
 * framework no-op, a duty-mode restart could have its predecessor's late
 * disable stop its successor, and a stale success callback could resurrect a
 * dead advertiser, all silently and all leaving this phone undiscoverable
 * with nothing logged. Each start now registers a *fresh* callback tagged
 * with a generation number, and every stop names exactly the generation that
 * started. [BleAdvertiserStateMachine] owns those decisions (and carries the
 * three field reproductions); everything here is binder glue. It also owns the
 * recovery those generations need -- a start that fails, or that the framework
 * never answers at all, re-arms on a capped backoff via [advertiseWatchdog],
 * because a phone that is not advertising gets no link events and so has
 * nothing else left to re-trigger it.
 *
 * Inbound admission + post-reject cooldown (2026-08-07): two asymmetries were
 * left over from the above. First, only the *outbound* half of the dual role
 * was budgeted -- [BleCentral]'s `MAX_CENTRAL_LINKS` -- so inbound centrals
 * could quietly consume the ACL headroom the central role was leaving free.
 * [PeripheralLinkAdmission] now caps them at [MAX_PERIPHERAL_LINKS]; a central
 * turned away at the margin is disconnected before it is ever tracked here,
 * and every already-established link is immune. Second, the notify-reject
 * teardown path below wiped an address's state and re-advertised immediately,
 * so the same central could reconnect on its next scan hit and re-trigger the
 * identical multi-KB HELLO/digest burst that had just broken the link.
 * [PeripheralSprayCooldown] gives that address a short window in which the
 * connection is still welcome but the burst is deferred; [MeshService] reads it
 * via [syncSprayDeferralMs] and re-arms the deferred sync through the same
 * coalescing resume the failover debounce uses.
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

    // Written under [advertiseLock] (start/stop) but read from GATT binder
    // threads under [lock] (sendFragment), so the publication has to be
    // visible without holding advertiseLock.
    @Volatile
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

    /** Inbound-link cap; see [MAX_PERIPHERAL_LINKS]. Leaf-synchronized itself. */
    private val linkAdmission = PeripheralLinkAdmission(MAX_PERIPHERAL_LINKS)

    /**
     * Post-notify-reject spray brake; see [PeripheralSprayCooldown]. Armed
     * here, read by [MeshService] through [syncSprayDeferralMs]. Deliberately
     * *not* cleared by [tearDownLink]: it is armed by a teardown and consulted
     * on the next connection, so surviving the teardown is the whole point.
     */
    private val sprayCooldown = PeripheralSprayCooldown()

    /**
     * Centrals turned away by [linkAdmission] that have not delivered their
     * STATE_DISCONNECTED yet. A rejected central is disconnected asynchronously
     * (the binder call cannot be made from inside the connect callback), so it
     * can still get a few GATT requests in first; without this, each of those
     * would allocate per-address state that nothing ever cleans up, because
     * [tearDownLink] no-ops for an address that was never tracked. Membership
     * here -- rather than absence from [connectedDevices] -- is the guard, so
     * every device this class did *not* reject is handled exactly as before.
     * Guarded by [lock].
     */
    private val rejectedCentrals = mutableSetOf<String>()

    /** Monotonic ms of the last at-cap log; throttles it per [AT_CAP_LOG_INTERVAL_MS]. Guarded by [lock]. */
    private var lastAtCapLogMs = 0L

    // Guards every read-modify-write of the per-address state above
    // (connectedDevices, negotiatedMtu, reassemblers, notifyQueues,
    // notifyInFlight, notifyFrameStarted, inFlightFragment). GATT server
    // callbacks arrive on arbitrary binder threads -- the 2026-07-17 field
    // log shows onNotificationSent for ONE address delivered on two
    // different binder threads while MeshService queued frames from a third
    // -- so the unguarded check-then-act on notifyInFlight could let several
    // notifications go out concurrently for one address, saturating the
    // controller (the likely trigger of the status=129 burst itself). Lock
    // ordering: only leaf locks (NotifyFailureTracker's / MeshRouter's) are
    // ever taken while holding this one, so callouts to
    // onCentralDisconnected under it cannot deadlock.
    private val lock = Any()

    // Paced sends fire on the main looper (mirrors BleCentral's
    // connect-watchdog Handler); the callback still takes [lock] before
    // touching state -- the Looper choice is about framework-call affinity,
    // not mutual exclusion.
    private val handler = Handler(Looper.getMainLooper())

    /**
     * All advertising decisions (including which [RadioDutyMode] the next
     * generation is built with) -- see [BleAdvertiserStateMachine].
     */
    private val advertiseMachine = BleAdvertiserStateMachine()

    /**
     * The one [AdvertiseCallback] instance registered with the framework per
     * generation, so a stop can pass back exactly the object that started --
     * `BluetoothLeAdvertiser` looks its advertising sets up by callback
     * identity, which is the entire reason generations exist. An entry is
     * removed the moment its generation is stopped, fails to start, or
     * reports late as stale, so this map holds at most the live generation.
     */
    private val advertiseCallbacks = mutableMapOf<Long, AdvertiseCallback>()

    /**
     * Guards [advertiseMachine] + [advertiseCallbacks] + the framework calls
     * that apply a decision, so two threads cannot interleave a stop and a
     * start and leave the framework in an order the state machine never
     * decided. Lock ordering: [lock] may be held while taking this (e.g.
     * [tearDownLink] re-arming advertising), never the reverse -- nothing
     * under this lock touches per-address state.
     */
    private val advertiseLock = Any()

    /**
     * The one pending [BleAdvertiserStateMachine.onWatchdogDue] tick, kept as a
     * single instance so [scheduleAdvertiseWatchdog] can replace it rather than
     * pile ticks up. It is what makes the machine's self-recovery real: a start
     * that fails, and a start the framework never answers at all, both leave
     * this armed instead of leaving the phone silently undiscoverable until
     * some unrelated link event happens to come along (which, for a phone that
     * is not advertising, may be never).
     */
    private val advertiseWatchdog = Runnable {
        synchronized(advertiseLock) {
            applyAdvertiseAction(advertiseMachine.onWatchdogDue(SystemClock.elapsedRealtime()))
        }
    }

    /**
     * Fresh per start. Its [generation] is what makes a late callback from a
     * retired advertising set identifiable and therefore ignorable.
     */
    private inner class GenerationAdvertiseCallback(private val generation: Long) : AdvertiseCallback() {
        override fun onStartSuccess(settingsInEffect: AdvertiseSettings) {
            synchronized(advertiseLock) {
                if (!advertiseMachine.acceptsResultFor(generation)) {
                    advertiseCallbacks.remove(generation)
                    Log.i(TAG, "Ignoring advertising start success from retired generation $generation")
                    return
                }
                Log.i(TAG, "Advertising started (generation $generation)")
                applyAdvertiseAction(advertiseMachine.onStartSucceeded(generation, SystemClock.elapsedRealtime()))
            }
        }

        override fun onStartFailure(errorCode: Int) {
            synchronized(advertiseLock) {
                if (!advertiseMachine.acceptsResultFor(generation)) {
                    advertiseCallbacks.remove(generation)
                    Log.i(TAG, "Ignoring advertising start failure $errorCode from retired generation $generation")
                    return
                }
                advertiseCallbacks.remove(generation)
                if (errorCode == ADVERTISE_FAILED_ALREADY_STARTED) {
                    // Unreachable by construction now that every start
                    // registers a callback the framework has never seen. It is
                    // logged loudly rather than swallowed because the old code
                    // quietly translated this code into "we're advertising",
                    // which is exactly how a dark radio went unnoticed for 15
                    // central connects.
                    Log.w(
                        TAG,
                        "Advertising failed: ADVERTISE_FAILED_ALREADY_STARTED for a fresh callback " +
                            "(generation $generation) -- unexpected; treating as not advertising",
                    )
                } else {
                    Log.w(TAG, "Advertising failed: $errorCode (generation $generation)")
                }
                applyAdvertiseAction(advertiseMachine.onStartFailed(generation, SystemClock.elapsedRealtime()))
            }
        }
    }

    private val gattServerCallback = object : BluetoothGattServerCallback() {
        override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
            Log.i(TAG, "Central ${device.address} connection state=$newState")
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> {
                    val decision = admitCentral(device)
                    // Legacy connectable advertising auto-stops the instant a
                    // central connects. Without restarting it, this phone goes
                    // dark to every other peer for the rest of the process
                    // (observed live 2026-07-11: the first peer to connect took
                    // the only peripheral slot and inbound 1:1 delivery died).
                    // The stack has already stopped the advertisement, so this
                    // retires the current generation (unregistering its
                    // callback with it) and starts a brand-new one, rather than
                    // re-asking the framework to start a set it already knows
                    // about -- which is what silently answered
                    // ADVERTISE_FAILED_ALREADY_STARTED until 2026-08-07.
                    //
                    // This runs for a *rejected* central too, and must: the
                    // framework stopped the advertisement for a connection we
                    // are about to drop, so skipping the restart here would
                    // make being at cap the one thing that reliably turns this
                    // phone dark. A phone at cap stays discoverable on purpose
                    // -- see PeripheralLinkAdmission.
                    restartAdvertisingAfterConnect()
                    if (decision is PeripheralAdmissionDecision.Rejected) rejectCentral(device, decision)
                }
                BluetoothProfile.STATE_DISCONNECTED -> {
                    synchronized(lock) { rejectedCentrals.remove(device.address) }
                    tearDownLink(device.address, "status=$status")
                }
            }
        }

        override fun onMtuChanged(device: BluetoothDevice, mtu: Int) {
            Log.i(TAG, "MTU negotiated for ${device.address}: $mtu")
            synchronized(lock) {
                if (device.address in rejectedCentrals) return
                negotiatedMtu[device.address] = mtu
            }
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
            Log.d(TAG, "Write request from ${device.address} for ${characteristic.uuid} (${value.size} bytes)")
            // A central we already turned away at the cap: answer the request
            // so it isn't left hanging on a link that is about to close, but
            // don't allocate a reassembler for it -- nothing would ever clean
            // that up, since tearDownLink no-ops for an untracked address.
            if (synchronized(lock) { device.address in rejectedCentrals }) {
                if (responseNeeded) {
                    gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, null)
                }
                return
            }
            if (characteristic.uuid == MeshConstants.INBOUND_CHARACTERISTIC_UUID) {
                // Only the map access needs the lock; GATT write requests on
                // one connection are request/response-serialized, so the
                // reassembler itself sees them in order. Reassembly and frame
                // handling stay outside the lock so inbound processing never
                // serializes against the send paths.
                val reassembler = synchronized(lock) {
                    reassemblers.getOrPut(device.address) { FrameReassembler() }
                }
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
            if (isOutboundCccdEnable && !synchronized(lock) { device.address in rejectedCentrals }) {
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
            synchronized(lock) {
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
                        sprayCooldown.armAfterRejectTeardown(address, SystemClock.elapsedRealtime())
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
    }

    /**
     * Decides whether a freshly connected central may hold one of this phone's
     * [MAX_PERIPHERAL_LINKS] inbound links, and publishes it into
     * [connectedDevices] only if so -- an untracked device is one this class
     * will not notify, will not reassemble frames for, and will not report as
     * connected, which is exactly what a refusal should mean.
     *
     * No binder call happens here, and none may: this runs on the connect
     * callback itself, where `cancelConnection` is both a call under [lock] and
     * a call into the stack from inside its own dispatch. [rejectCentral] does
     * that part, after the lock is released and off this callback.
     */
    private fun admitCentral(device: BluetoothDevice): PeripheralAdmissionDecision {
        val address = device.address
        return synchronized(lock) {
            // A reconnect under an address we previously rejected is a brand
            // new admission decision, not a continuation of that rejection.
            rejectedCentrals.remove(address)
            val decision = linkAdmission.admit(address)
            when (decision) {
                is PeripheralAdmissionDecision.Rejected -> rejectedCentrals += address
                is PeripheralAdmissionDecision.Admitted,
                is PeripheralAdmissionDecision.AlreadyHeld,
                -> connectedDevices[address] = device
            }
            decision
        }
    }

    /**
     * Drops a central admitted by nobody. Posted rather than called inline:
     * `cancelConnection` is a binder call, and issuing it from inside
     * `onConnectionStateChange` re-enters the stack during its own connect
     * dispatch. Posting it also keeps the binder call off [lock] entirely
     * (the teardown-under-lock shape flagged in the #269 review).
     */
    private fun rejectCentral(device: BluetoothDevice, decision: PeripheralAdmissionDecision.Rejected) {
        val shouldLog = synchronized(lock) {
            val nowMs = SystemClock.elapsedRealtime()
            (nowMs - lastAtCapLogMs >= AT_CAP_LOG_INTERVAL_MS).also { if (it) lastAtCapLogMs = nowMs }
        }
        if (shouldLog) {
            Log.i(
                TAG,
                "At inbound link cap (${decision.activeCount}/$MAX_PERIPHERAL_LINKS); turning away newly " +
                    "connected centrals (e.g. ${device.address}) until a slot frees -- established links are " +
                    "kept, and this phone stays discoverable",
            )
        }
        handler.post {
            runCatching { gattServer?.cancelConnection(device) }
                .onFailure { Log.w(TAG, "cancelConnection for a rejected central failed: ${it.message}") }
        }
    }

    /**
     * How much longer the HELLO-triggered carry-drain + digest burst to
     * [address] must be held back, or 0 when it may go out now. Read by
     * [MeshService] at HELLO time; see [PeripheralSprayCooldown] for why the
     * connection itself is never refused and why a deferred burst is re-armed
     * rather than dropped.
     */
    fun syncSprayDeferralMs(address: String): Long =
        sprayCooldown.deferralMs(address, SystemClock.elapsedRealtime())

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

        val server = bluetoothManager.openGattServer(context, gattServerCallback)?.also {
            it.addService(buildGattService())
        }

        // Publishing the server and asking for the first advertising generation
        // happen under one lock so a teardown racing in between cannot start a
        // generation against a server this method is still installing (the
        // mirror of the window stop() closes).
        synchronized(advertiseLock) {
            gattServer = server
            advertiser = btAdapter.bluetoothLeAdvertiser
            beginAdvertising()
        }
    }

    /**
     * (Re)starts connectable advertising unless it is already running or a
     * start is already in flight. Called from [start] and after every link
     * teardown, so the peripheral stays discoverable for additional and
     * subsequent centrals instead of going dark after its first connection.
     * [BleAdvertiserStateMachine] absorbs the redundant calls (e.g. a teardown
     * while other links are still up) so they can't thrash the advertiser.
     *
     * A central connect uses [restartAdvertisingAfterConnect] instead: there
     * the framework has already stopped the set underneath us, so the current
     * generation has to be retired rather than left alone.
     */
    private fun beginAdvertising() {
        synchronized(advertiseLock) {
            applyAdvertiseAction(advertiseMachine.onStartRequested(SystemClock.elapsedRealtime()))
        }
    }

    /** See [BleAdvertiserStateMachine.onConnectRestartRequested]. */
    private fun restartAdvertisingAfterConnect() {
        synchronized(advertiseLock) {
            applyAdvertiseAction(advertiseMachine.onConnectRestartRequested(SystemClock.elapsedRealtime()))
        }
    }

    /**
     * Runs one [AdvertiserAction] against the framework: the stop always
     * passes back the exact [AdvertiseCallback] instance its generation was
     * started with (`BluetoothLeAdvertiser` looks its advertising sets up by
     * callback identity), the start registers a brand-new one, and a requested
     * watchdog is (re)armed on [handler].
     *
     * Callers must hold [advertiseLock].
     */
    private fun applyAdvertiseAction(action: AdvertiserAction) {
        action.stopGeneration?.let { generation ->
            val callback = advertiseCallbacks.remove(generation)
            if (callback != null) {
                // stopAdvertising throws if the adapter is already off -- the
                // case when this runs because Bluetooth was turned off.
                // Swallow it; that generation is gone either way.
                try {
                    advertiser?.stopAdvertising(callback)
                } catch (e: Exception) {
                    Log.w(
                        TAG,
                        "stopAdvertising for generation $generation failed (adapter likely off): ${e.message}",
                    )
                }
            }
        }
        // The watchdog is armed *before* the start: a synchronous failure
        // inside startAdvertisingGeneration re-enters here with the retry's own
        // (shorter) watchdog, and whichever runs last is the one that stays
        // armed.
        action.watchdogInMs?.let(::scheduleAdvertiseWatchdog)
        action.startGeneration?.let(::startAdvertisingGeneration)
    }

    /**
     * Arms the single [advertiseWatchdog] tick, replacing any pending one --
     * the state machine always asks for the one deadline that matters next, so
     * there is never a second one worth keeping. Callers must hold
     * [advertiseLock].
     */
    private fun scheduleAdvertiseWatchdog(delayMs: Long) {
        handler.removeCallbacks(advertiseWatchdog)
        handler.postDelayed(advertiseWatchdog, delayMs.coerceAtLeast(0))
    }

    /** Callers must hold [advertiseLock]; see [applyAdvertiseAction]. */
    private fun startAdvertisingGeneration(generation: Long) {
        // Don't advertise a torn-down server: stop() nulls gattServer, and a
        // late STATE_DISCONNECTED callback must not resurrect advertising.
        // Reporting the non-start back to the state machine matters as much as
        // not starting: leaving it in STARTING forever would early-return every
        // later restart, which is the shape of the very bug this file is
        // fixing. This is a *stop*, not a failure -- there is no peripheral
        // role to be discoverable for, so retrying on a timer would be a
        // pointless wakeup every minute for as long as the mesh is off.
        val adv = advertiser
        if (gattServer == null || adv == null) {
            Log.i(TAG, "Not starting advertising generation $generation: peripheral role is not running")
            applyAdvertiseAction(advertiseMachine.onStopRequested())
            return
        }
        val settings = AdvertiseSettings.Builder()
            // BALANCED (restored from LOW_POWER 2026-07-10) vs LOW_POWER, now
            // adaptive per the state machine's desired mode (battery,
            // 2026-07-21): the longer LOW_POWER advertising interval made this
            // peer hard for a central to catch for a direct connect (status=133
            // churn / slow first connect), which is exactly why
            // [RadioPowerPolicy] favors BALANCED while lonely or right after a
            // link change and only relaxes to LOW_POWER once a link is up and
            // stays quiet -- see [setAdvertiseDutyMode]. The mesh no longer
            // pauses for Bluetooth audio, so BALANCED periods still favor a
            // faster, more catchable advertisement exactly as the 2026-07-10
            // fix intended.
            // TX power stays MEDIUM -- that governs range (ship-scale mesh); it's
            // the advertising *interval* (the mode) that drives coexistence.
            .setAdvertiseMode(
                when (advertiseMachine.desiredMode()) {
                    RadioDutyMode.LOW_POWER -> AdvertiseSettings.ADVERTISE_MODE_LOW_POWER
                    RadioDutyMode.BALANCED -> AdvertiseSettings.ADVERTISE_MODE_BALANCED
                },
            )
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
        val callback = GenerationAdvertiseCallback(generation)
        advertiseCallbacks[generation] = callback
        try {
            adv.startAdvertising(settings, data, scanResponse, callback)
        } catch (e: Exception) {
            // Same reasoning as the null-adapter branch: a synchronous throw
            // (adapter turned off mid-call) means no callback will ever arrive,
            // so the generation has to be retired here or nothing ever
            // advertises again.
            Log.w(TAG, "startAdvertising for generation $generation threw: ${e.message}")
            advertiseCallbacks.remove(generation)
            // Unlike the no-server case above this *is* a failure: the
            // peripheral role is meant to be up, so it re-arms and tries again
            // (the adapter may simply be mid-toggle).
            applyAdvertiseAction(advertiseMachine.onStartFailed(generation, SystemClock.elapsedRealtime()))
        }
    }

    /**
     * Battery (2026-07-21): [MeshService] calls this with [RadioPowerPolicy]'s
     * latest decision -- see the class doc. A no-op unless [mode] actually
     * differs from what's already set, so callers can call this
     * unconditionally on every policy tick without thrashing the advertiser.
     * When not currently advertising, only the desired mode updates -- the
     * next started generation picks it up. When already advertising, this
     * forces a stop-then-start restart (the same restart path a central
     * connect triggers for the legacy-advertising-stops-on-connect quirk,
     * PR#17) so the new [AdvertiseSettings] mode actually takes effect. When a
     * start is still in flight the restart is queued behind it rather than
     * dropped -- see [BleAdvertiserStateMachine.onDutyModeRequested].
     */
    fun setAdvertiseDutyMode(mode: RadioDutyMode) {
        val outcome = synchronized(advertiseLock) {
            if (advertiseMachine.desiredMode() == mode) return
            val action = advertiseMachine.onDutyModeRequested(mode, SystemClock.elapsedRealtime())
            applyAdvertiseAction(action)
            when {
                !action.isNone -> "applied"
                advertiseMachine.hasRestartPending() -> "queued behind an in-flight advertising start"
                else -> "recorded for the next advertising start"
            }
        }
        Log.i(TAG, "setAdvertiseDutyMode: advertise duty mode $mode $outcome")
    }

    fun stop() {
        synchronized(advertiseLock) {
            applyAdvertiseAction(advertiseMachine.onStopRequested())
            // Belt and braces: onStopRequested retires the live generation, so
            // this should already be empty. Anything left is a callback the
            // framework owes us a result for that can now only report as
            // stale, and holding it would just leak the BlePeripheral it
            // closes over.
            advertiseCallbacks.clear()
            // Closing and un-publishing the server belongs under the same lock
            // as the stop decision. Released early, it leaves a window where a
            // GATT binder thread delivering STATE_DISCONNECTED runs
            // tearDownLink -> beginAdvertising, sees IDLE and a still-non-null
            // gattServer, and starts a generation against a server this method
            // is about to close -- leaving the machine STARTING for an
            // advertiser that cannot exist, which absorbs the next start().
            runCatching { gattServer?.close() }
            gattServer = null
        }
        handler.removeCallbacksAndMessages(null)
        synchronized(lock) {
            connectedDevices.clear()
            negotiatedMtu.clear()
            reassemblers.clear()
            notifyQueues.clear()
            notifyInFlight.clear()
            notifyFrameStarted.clear()
            inFlightFragment.clear()
            notifyFailures.clearAll()
            rejectedCentrals.clear()
            linkAdmission.clearAll()
            // The peripheral role is going away entirely; there is no
            // reconnect for a cooldown to brake, and a restart starts clean.
            sprayCooldown.clearAll()
            lastAtCapLogMs = 0L
        }
    }

    /**
     * Push a frame to every subscribed central via the notify characteristic,
     * fragmenting per each central's negotiated MTU if needed (DESIGN.md
     * §5.2).
     */
    fun notifyFrame(frame: ByteArray) {
        synchronized(lock) {
            connectedDevices.values.toList()
        }.forEach { device ->
            sendFrame(device.address, frame)
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
        synchronized(lock) {
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
            Log.d(TAG, "sendFrame: queued ${fragments.size} fragment(s) for $deviceAddress (${frame.size} bytes)")
            sendNextQueuedFragment(device)
        }
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
     *
     * Callers must hold [lock] (all current callers do; the monitor is
     * reentrant, so calls from already-locked paths are fine) -- the
     * check-then-act on [notifyInFlight] below is exactly the race that lets
     * two threads each see the address as idle and put two notifications in
     * flight at once.
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
        // one-notification-per-connection GATT constraint. That reservation
        // is also what makes the paced callback below race-free: any retry
        // or new queueing for this address between now and the delayed
        // firing sees the slot taken and backs off, so the delayed
        // sendFragment can never double-send alongside another path.
        notifyInFlight += address
        val queuedFrames = frames.size
        if (shouldPaceFrameStart(startingNewFrame, queuedFrames)) {
            handler.postDelayed({
                synchronized(lock) {
                    if (address in connectedDevices) {
                        sendFragment(device, fragment)
                    } else {
                        notifyInFlight.remove(address)
                    }
                }
            }, FRAME_PACING_DELAY_MS)
        } else {
            sendFragment(device, fragment)
        }
    }

    /** Callers must hold [lock]; see [sendNextQueuedFragment]. */
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
            sprayCooldown.armAfterRejectTeardown(address, SystemClock.elapsedRealtime())
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
     *
     * Takes [lock] itself (reentrant for already-locked callers) so the
     * STATE_DISCONNECTED callback path is guarded too, and the idempotence
     * check-then-act on [connectedDevices] can't race a concurrent teardown.
     */
    private fun tearDownLink(address: String, reason: String) {
        synchronized(lock) {
            if (address !in connectedDevices) return
            Log.i(TAG, "tearDownLink: $address ($reason)")
            connectedDevices.remove(address)
            // Free this address's inbound slot for whoever connects next. Only
            // a link that genuinely went away frees one -- the cap never evicts
            // (see PeripheralLinkAdmission).
            linkAdmission.release(address)
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
