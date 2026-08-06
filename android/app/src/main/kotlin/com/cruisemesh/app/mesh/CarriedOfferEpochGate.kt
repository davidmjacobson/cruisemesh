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
    )

    private var initialized = false
    private var epochStartMs = 0L
    private var offersThisEpoch = 0
    private var nextReservationId = 0L
    private val uncommitted = mutableSetOf<Long>()

    @Synchronized
    fun tryReserve(nowMs: Long): Reservation? {
        resetEpochIfNeeded(nowMs)
        if (!mayStartCarriedOffer(offersThisEpoch.toUInt())) return null
        nextReservationId = if (nextReservationId == Long.MAX_VALUE) 0L else nextReservationId + 1L
        offersThisEpoch += 1
        uncommitted += nextReservationId
        return Reservation(nextReservationId, epochStartMs)
    }

    @Synchronized
    fun commit(reservation: Reservation) {
        if (reservation.epochStartMs == epochStartMs) {
            uncommitted.remove(reservation.id)
        }
    }

    @Synchronized
    fun release(reservation: Reservation) {
        if (reservation.epochStartMs == epochStartMs && uncommitted.remove(reservation.id)) {
            offersThisEpoch -= 1
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
        }
    }
}
