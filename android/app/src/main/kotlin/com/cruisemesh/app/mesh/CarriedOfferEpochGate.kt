package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreCarriedOfferGate
import uniffi.cruisemesh_core.CoreCarriedOfferReservation
import uniffi.cruisemesh_core.coreCarriedOfferEpochMs

/**
 * Thin shell wrapper over the core's shared foreign-carry allowance (see
 * `CoreCarriedOfferGate` for the cap, the epoch length and the logical-peer
 * rule). iOS wraps the same core object, so neither the window nor the
 * allowance can drift between the platforms — the same arrangement as
 * [ReconnectBackoffTracker].
 *
 * Reservations are made before a digest plan is built. A non-empty plan is
 * [commit]ted and continues to count for the epoch; an empty or failed plan is
 * [release]d so it does not consume another peer's allowance.
 */
internal class CarriedOfferEpochGate(
    epochMs: Long = defaultEpochMs,
) {
    companion object {
        val defaultEpochMs: Long get() = coreCarriedOfferEpochMs()
    }

    private val core = CoreCarriedOfferGate.withEpochMs(epochMs)

    val epochMs: Long get() = core.epochMs()

    /**
     * Claims one of the epoch's slots, or `null` when the allowance is spent or
     * this logical peer already had its offer. [logicalPeerId] is the peer's
     * UserID hex, never a link address.
     */
    fun tryReserve(nowMs: Long, logicalPeerId: String? = null): CoreCarriedOfferReservation? =
        core.tryReserve(nowMs, logicalPeerId)

    fun commit(reservation: CoreCarriedOfferReservation) = core.commit(reservation)

    fun release(reservation: CoreCarriedOfferReservation) = core.release(reservation)
}
