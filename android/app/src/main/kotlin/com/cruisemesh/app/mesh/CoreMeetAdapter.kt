package com.cruisemesh.app.mesh

import android.util.Log
import uniffi.cruisemesh_core.CoreCarriedOfferGate
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreMeetOutcome
import uniffi.cruisemesh_core.CoreMeetRequest
import uniffi.cruisemesh_core.CoreMeetWork
import uniffi.cruisemesh_core.CoreMeshRouterState
import uniffi.cruisemesh_core.CoreSprayPolicy
import uniffi.cruisemesh_core.CoreSprayTrigger
import uniffi.cruisemesh_core.MessageStore

private const val TAG = "MeshService"

/**
 * The driver half of the encounter path: hand one meeting to the single core
 * planner, then put the frames it hands back on the radio in the order it
 * returned them.
 *
 * What core decides here, and this class does not: whether this link owes a
 * DIGEST at all and what goes in it, which carried envelopes the peer's
 * advertised ids prove it already holds (and so may be retired), which
 * remaining carries are hint-matched for this peer, how many bytes each lane
 * may spend, whether the cadence gate allows a mule spray this encounter,
 * whether the device's per-epoch third-party offer allowance has a slot left,
 * and the order the three lanes go out in. Every one of those used to be
 * re-derived in Kotlin beside a second copy in Swift and a third in the
 * simulator; it now lives once in `core/src/session/mesh_meet.rs`.
 *
 * What this class keeps is what a driver is for: the transports, the send
 * itself, and the logging.
 *
 * ## Two clocks, on purpose
 *
 * The store, the router windows and the offer epoch are measured on the wall
 * clock, exactly as the legacy sequencing measured them. The spray cadence is
 * measured on [SprayPolicy.nowMs] -- `SystemClock.elapsedRealtime` -- because
 * an NTP correction landing mid-session must not expire a spray window early
 * and buy the burst the window exists to prevent. The planner takes both and
 * hands each to the state that counts with it; this adapter is where the two
 * are read, one per encounter, so no lane can see them drift apart mid-plan.
 *
 * ## What core is NOT given
 *
 * Nothing about link health, and no return value through which it could pause
 * a transport. The planner decides what this encounter offers, never that a
 * radio or the relay is unnecessary.
 */
internal class CoreMeetAdapter(
    private val store: MessageStore,
    private val router: CoreMeshRouterState,
    private val spray: CoreSprayPolicy,
    private val offers: CoreCarriedOfferGate,
    private val links: Links,
    private val now: () -> Long = System::currentTimeMillis,
    private val sprayNow: () -> Long = SprayPolicy::nowMs,
) {

    /**
     * The one typed action core hands back. Every frame is fully encoded by
     * core; nothing here infers an ordering, a budget or a retry of its own.
     */
    interface Links {
        /** Put one already-encoded frame on [address]. */
        fun send(address: String, frame: ByteArray)
    }

    /**
     * Runs one encounter through the core planner and sends what it returns.
     *
     * Returns the planner's bounded work counts, or `null` when the plan
     * itself failed -- in which case nothing was sent and nothing was
     * recorded, because every store mutation the planner makes commits inside
     * the same call that would have produced the frames.
     *
     * `peerAuthenticated` must reflect the transport the peer's claim ARRIVED
     * on (CARRY-02), not the link this encounter answers on: a digest that
     * arrived over unauthenticated BLE and is replayed on a freshly elected
     * LAN link must not have its advertised ids laundered into an
     * authenticated removal.
     */
    fun encounter(
        address: String,
        ownUserId: ByteArray,
        peerUserId: ByteArray,
        trigger: CoreSprayTrigger,
        peerKnownMsgIds: List<ByteArray> = emptyList(),
        peerAuthenticated: Boolean = false,
        peerCapabilities: UInt? = null,
    ): CoreMeetWork? {
        val outcome = try {
            store.corePlanMeshMeet(
                router,
                spray,
                offers,
                CoreMeetRequest(
                    ownUserId = ownUserId,
                    peerUserId = peerUserId,
                    peerAddress = address,
                    peerKnownMsgIds = peerKnownMsgIds,
                    peerAuthenticated = peerAuthenticated,
                    peerCapabilities = peerCapabilities,
                    trigger = trigger,
                    nowMs = now(),
                    sprayNowMs = sprayNow(),
                ),
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Encounter plan for $address failed (${e.message}); nothing sent")
            return null
        }
        send(address, outcome)
        return outcome.work
    }

    /**
     * Digest, then targeted drain, then spray -- the field order in the order
     * the planner returned it.
     *
     * It is load-bearing, not cosmetic: core's exchange window opens when the
     * digest is enqueued, and on a slow BLE link a multi-KB drain queued ahead
     * of it holds it in the FIFO past the window, so the peer's answer arrives
     * to a shut gate. Iterating the three lists in field order is what makes
     * that ordering something the planner enforces instead of a convention
     * every call site had to remember.
     */
    private fun send(address: String, outcome: CoreMeetOutcome) {
        for (frame in outcome.digestFrames) links.send(address, frame)
        for (frame in outcome.targetedFrames) links.send(address, frame)
        for (frame in outcome.sprayFrames) links.send(address, frame)
    }
}
