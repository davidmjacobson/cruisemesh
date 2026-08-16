package com.cruisemesh.app.mesh

import android.os.SystemClock
import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.CoreSprayAdmission
import uniffi.cruisemesh_core.CoreSprayGate
import uniffi.cruisemesh_core.CoreSprayPlanShape
import uniffi.cruisemesh_core.CoreSprayPolicy
import uniffi.cruisemesh_core.CoreSprayTrigger
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreSprayRetryArmMaxMs

/**
 * Process-wide delegate to the core digest-spray policy (same singleton shape
 * as [MeshRouter]).
 *
 * There is deliberately no decision in this file. Every question this shell
 * used to answer with a local timestamp -- may this peer be sprayed, how much
 * may go out, is this the same offer we just made, how long should a quiet
 * link wait -- is answered by `core/src/spray_policy.rs`, so Android and iOS
 * cannot drift. What lives here is the address-to-key mapping, the clock
 * choice, and nothing else.
 *
 * ## Keys
 *
 * - `peerKey` is the hex user id: the *logical* peer. Cadence is per peer
 *   because reconnect churn moves between addresses, and keying it by address
 *   would let a phone that reconnects on a new address bypass the gate.
 * - `linkKey` is the transport address. The byte budget is per link because
 *   the FIFO being filled belongs to a link, not to a peer.
 *
 * ## Clock
 *
 * [SystemClock.elapsedRealtime], never `System.currentTimeMillis`. The map
 * this replaces used the wall clock, so an NTP correction landing mid-session
 * could expire a spray window early -- producing exactly the burst the window
 * exists to prevent -- or hold it open indefinitely. It is also the clock
 * `postDelayed`, [FailoverResumeDebounce] and [PeripheralSprayCooldown] all
 * count on, so the three gates now measure time the same way.
 */
object SprayPolicy {
    private val core = CoreSprayPolicy()

    /**
     * The shared Rust policy itself, for `MessageStore.corePlanMeshMeet`,
     * which takes the cadence verdict and charges the burst allowance inside
     * one planning call. It has to be this object: a planner given a fresh
     * policy would forget every window the moment it returned.
     */
    internal val coreState: CoreSprayPolicy get() = core

    /** Monotonic milliseconds; see the class KDoc. */
    fun nowMs(): Long = SystemClock.elapsedRealtime()

    /**
     * May this peer be sprayed now, and with what per-lane byte budgets?
     *
     * Consulted before any store work, so a reconnect storm costs a map
     * lookup rather than a full plan build. Records nothing: a burst the
     * post-reject cooldown then defers must not arm a cadence it never spent.
     */
    fun maySpray(
        peerUserId: ByteArray,
        address: String,
        trigger: CoreSprayTrigger,
        nowMs: Long = nowMs(),
    ): CoreSprayGate = core.maySpray(UserIdHex.encode(peerUserId), address, trigger, nowMs)

    /** A DIGEST frame actually went out to this peer on this link. */
    fun noteDigestSent(peerUserId: ByteArray, address: String, nowMs: Long = nowMs()) {
        core.noteDigestSent(UserIdHex.encode(peerUserId), address, nowMs)
    }

    /**
     * A plan is built; which of its lanes go on the radio?
     *
     * Per lane, not per plan: the recorded shape was an invariant authored set
     * beside a carried set walking a cursor, and one digest over all three
     * would change on every page turn and so suppress nothing.
     *
     * When a lane is refused the caller must not send it, must not advance a
     * carried cursor, and must not record hidden-kind offers -- a suppressed
     * offer has to stay exactly as re-discoverable as it was.
     */
    fun admitPlan(
        peerUserId: ByteArray,
        address: String,
        lanes: CoreSprayPlanShape,
        nowMs: Long = nowMs(),
    ): CoreSprayAdmission = core.admitPlan(UserIdHex.encode(peerUserId), address, lanes, nowMs)

    /**
     * Bytes this encounter queued at [address] outside a spray plan: the
     * receipt repair pass, the per-missing-message re-send loop, the group
     * catch-up and the carry drain. Pure accounting -- it refuses nothing, it
     * changes what the next [maySpray] sees.
     */
    fun noteBytesQueued(address: String, bytes: Long, nowMs: Long = nowMs()) {
        if (bytes <= 0L) return
        core.noteBytesQueued(address, bytes.toULong(), nowMs)
    }

    /**
     * Evidence that sprays toward this peer are achieving something: carried
     * copies it confirmed holding, or a receipt consumed from it. Resets the
     * receipt-quiet backoff.
     */
    fun noteReceiptProgress(peerUserId: ByteArray, nowMs: Long = nowMs()) {
        core.noteReceiptProgress(UserIdHex.encode(peerUserId), nowMs)
    }

    /** Longest deferral worth arming a timer for, from core. */
    fun retryArmMaxMs(): Long = coreSprayRetryArmMaxMs()

    /**
     * A link went away. Nothing is reset: neither the peer's cadence nor this
     * link's burst allowance. A disconnect is what reconnect churn produces --
     * hundreds per hour in the field -- so clearing either on one would hand
     * the churn back the bound it defeats.
     */
    fun noteLinkClosed(address: String, nowMs: Long = nowMs()) {
        core.noteLinkClosed(address, nowMs)
    }

    /**
     * Route this policy's decisions into the store's protocol-event ring, so a
     * shared diagnostics archive can show why a peer was sprayed, suppressed
     * or backed off.
     *
     * Idempotent, and safe to call late: an unattached policy behaves exactly
     * as it did before the ring existed. Core builds the records and redacts
     * them; nothing here composes an event.
     */
    fun attachEventJournal(store: MessageStore) = core.attachEventJournal(store)

    /** Mesh stopped; none of this is durable state. */
    fun reset() = core.clear()
}
