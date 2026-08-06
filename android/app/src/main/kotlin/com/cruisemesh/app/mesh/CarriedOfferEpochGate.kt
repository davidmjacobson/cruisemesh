package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.mayStartCarriedOffer

/**
 * Atomically reserves the shared foreign-carry allowance for one short epoch.
 *
 * Reservations are made before a digest plan is built. A non-empty plan is
 * [commit]ted and continues to count for the epoch; an empty/failed plan is
 * [release]d so it does not consume another peer's allowance.
 */
internal class CarriedOfferEpochGate(
    private val epochMs: Long,
) {
    init {
        require(epochMs > 0)
    }

    class Reservation internal constructor(
        internal val id: Long,
        internal val epochStartMs: Long,
        internal val logicalPeerId: String?,
    )

    private var initialized = false
    private var epochStartMs = 0L
    private var offersThisEpoch = 0
    private var nextReservationId = 0L
    private val uncommitted = mutableMapOf<Long, String?>()
    private val offeredLogicalPeers = mutableSetOf<String>()

    @Synchronized
    fun tryReserve(nowMs: Long, logicalPeerId: String? = null): Reservation? {
        resetEpochIfNeeded(nowMs)
        if (!mayStartCarriedOffer(offersThisEpoch.toUInt())) return null
        if (logicalPeerId != null && logicalPeerId in offeredLogicalPeers) return null
        nextReservationId = if (nextReservationId == Long.MAX_VALUE) 0L else nextReservationId + 1L
        offersThisEpoch += 1
        uncommitted[nextReservationId] = logicalPeerId
        if (logicalPeerId != null) offeredLogicalPeers += logicalPeerId
        return Reservation(nextReservationId, epochStartMs, logicalPeerId)
    }

    @Synchronized
    fun commit(reservation: Reservation) {
        if (reservation.epochStartMs == epochStartMs) {
            uncommitted.remove(reservation.id)
        }
    }

    @Synchronized
    fun release(reservation: Reservation) {
        if (reservation.epochStartMs == epochStartMs && uncommitted.containsKey(reservation.id)) {
            uncommitted.remove(reservation.id)
            offersThisEpoch -= 1
            reservation.logicalPeerId?.let(offeredLogicalPeers::remove)
        }
    }

    private fun resetEpochIfNeeded(nowMs: Long) {
        if (
            !initialized ||
            nowMs < epochStartMs ||
            nowMs - epochStartMs >= epochMs
        ) {
            initialized = true
            epochStartMs = nowMs
            offersThisEpoch = 0
            uncommitted.clear()
            offeredLogicalPeers.clear()
        }
    }
}
