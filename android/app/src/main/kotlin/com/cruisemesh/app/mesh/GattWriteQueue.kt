package com.cruisemesh.app.mesh

import java.util.ArrayDeque

/**
 * Pure, unit-testable per-address outbound-fragment queue and in-flight gate
 * backing [BleCentral]'s GATT characteristic writes -- extracted so the
 * check-then-act on "is a write already in flight for this address" can be
 * tested without any Android/BLE dependency, matching the
 * [NotifyFailureTracker]/[ReconnectBackoffTracker] leaf-class pattern
 * (TODO.md §3.4: "pure schedule/policy logic = plain classes, @Synchronized
 * leaf monitors, no Android imports, unit-tested directly").
 *
 * A BluetoothGatt allows only one in-flight characteristic write per
 * connection at a time (DESIGN.md §5.2). Before this class existed,
 * [BleCentral.sendNextQueuedFragment] implemented that constraint as a plain
 * check-then-act (`if (address in writeInFlight) return` followed by a
 * separate poll), unguarded by any lock -- exactly the race
 * [BlePeripheral]'s `lock` (see its class doc, "onNotificationSent for ONE
 * address delivered on two different binder threads") was already hardened
 * against; BleCentral never got the same fix, so two GATT callback threads
 * (or a callback thread racing a caller thread via [BleCentral.sendFrame])
 * could both observe "not in flight" and both issue a write for the same
 * address at once. [admitNext] closes that window by combining the in-flight
 * check, the FIFO pop, and the reservation into one atomic step: a caller
 * that gets back a non-null fragment is guaranteed to be the only one
 * holding that address's write slot until it calls [completeWrite].
 *
 * Methods are @Synchronized as a second line of defense; every call in
 * [BleCentral] today already happens under its own `lock`, so this stays
 * consistent with [NotifyFailureTracker]'s "leaf monitor, never calls out"
 * discipline (it cannot deadlock with the caller's lock).
 */
class GattWriteQueue {
    private val queues = mutableMapOf<String, ArrayDeque<ByteArray>>()
    private val inFlight = mutableSetOf<String>()

    /** Appends [fragments] to [address]'s outbound queue, in send order. */
    @Synchronized
    fun enqueue(address: String, fragments: List<ByteArray>) {
        queues.getOrPut(address) { ArrayDeque() }.addAll(fragments)
    }

    /**
     * Atomically checks the in-flight gate and pops the next fragment for
     * [address]. Returns null if a write is already in flight for that
     * address, or if the queue is empty -- in either case the caller must
     * not attempt a GATT write. When a fragment is returned, [address] is
     * immediately marked in-flight (the reservation happens here, before any
     * GATT call), so a concurrent caller racing this one sees the slot taken
     * and gets null instead of a fragment. The caller must eventually call
     * [completeWrite] (whether the write it performs afterward succeeds or
     * fails) to release the slot -- or [clear]/[clearAll] on teardown.
     */
    @Synchronized
    fun admitNext(address: String): ByteArray? {
        if (address in inFlight) return null
        val fragment = queues[address]?.poll() ?: return null
        inFlight += address
        return fragment
    }

    /**
     * Releases [address]'s in-flight slot so the next queued fragment (if
     * any) can be admitted. Safe to call whether the write that held the
     * slot succeeded or failed -- callers that are tearing the link down
     * entirely should call [clear] instead, which also drops the queue.
     */
    @Synchronized
    fun completeWrite(address: String) {
        inFlight.remove(address)
    }

    /** Forgets [address]'s queue and in-flight state, e.g. on link teardown. */
    @Synchronized
    fun clear(address: String) {
        queues.remove(address)
        inFlight.remove(address)
    }

    /** Forgets every address's state, e.g. on a full role [BleCentral.stop]. */
    @Synchronized
    fun clearAll() {
        queues.clear()
        inFlight.clear()
    }

    /** For diagnostics/tests: whether a write is currently in flight for [address]. */
    @Synchronized
    fun isInFlight(address: String): Boolean = address in inFlight

    /** For diagnostics/tests: how many fragments are queued (not counting one in flight) for [address]. */
    @Synchronized
    fun queuedCount(address: String): Int = queues[address]?.size ?: 0
}
