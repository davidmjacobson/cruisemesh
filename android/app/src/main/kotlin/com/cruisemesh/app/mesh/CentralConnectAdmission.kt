package com.cruisemesh.app.mesh

/**
 * Atomic admission for [BleCentral]'s worker-queued `connectGatt` calls.
 *
 * A reserved address consumes one of [maxActive] slots immediately, before
 * the framework call is posted. That keeps a burst of scan callbacks from all
 * observing the same stale `connections.size`, and the session carried by a
 * [Reservation] lets `stop()` invalidate work that is already queued or
 * inside `connectGatt` without killing the reusable worker thread.
 */
internal class CentralConnectAdmission(
    private val maxActive: Int,
) {
    init {
        require(maxActive > 0)
    }

    class Reservation internal constructor(
        val address: String,
        internal val session: Long,
    )

    data class Attempt(
        val reservation: Reservation?,
        val atCapacity: Boolean,
        val activeCount: Int,
    )

    private enum class Phase { PENDING, CONNECTING, CONNECTED }

    private data class Entry(
        val session: Long,
        var phase: Phase,
    )

    private var running = false
    private var session = 0L
    private val entries = mutableMapOf<String, Entry>()

    @Synchronized
    fun startSession() {
        if (running) return
        session = session.nextGeneration()
        running = true
        entries.clear()
    }

    @Synchronized
    fun stopSession() {
        running = false
        session = session.nextGeneration()
        entries.clear()
    }

    /** Reserve capacity for [address], including while its worker call waits. */
    @Synchronized
    fun tryReserve(address: String): Attempt {
        if (!running || address in entries) {
            return Attempt(null, atCapacity = false, activeCount = entries.size)
        }
        if (entries.size >= maxActive) {
            return Attempt(null, atCapacity = true, activeCount = entries.size)
        }
        entries[address] = Entry(session, Phase.PENDING)
        return Attempt(
            reservation = Reservation(address, session),
            atCapacity = false,
            activeCount = entries.size,
        )
    }

    /** Claim queued work immediately before making the framework call. */
    @Synchronized
    fun beginConnect(reservation: Reservation): Boolean {
        val entry = entryFor(reservation) ?: return false
        if (entry.phase != Phase.PENDING) return false
        entry.phase = Phase.CONNECTING
        return true
    }

    /** Publish a returned GATT only if this reservation's session is live. */
    @Synchronized
    fun completeConnect(reservation: Reservation): Boolean {
        val entry = entryFor(reservation) ?: return false
        if (entry.phase != Phase.CONNECTING) return false
        entry.phase = Phase.CONNECTED
        return true
    }

    /** Release work that was rejected by the handler or failed before publish. */
    @Synchronized
    fun cancel(reservation: Reservation) {
        val entry = entryFor(reservation) ?: return
        if (entry.phase != Phase.CONNECTED) {
            entries.remove(reservation.address)
        }
    }

    @Synchronized
    fun disconnect(address: String) {
        entries.remove(address)
    }

    private fun entryFor(reservation: Reservation): Entry? {
        if (!running || reservation.session != session) return null
        return entries[reservation.address]?.takeIf { it.session == reservation.session }
    }
}

private fun Long.nextGeneration(): Long = if (this == Long.MAX_VALUE) 0L else this + 1L
