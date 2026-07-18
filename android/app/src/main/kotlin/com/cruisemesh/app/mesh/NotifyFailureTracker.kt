package com.cruisemesh.app.mesh

/**
 * Pure, unit-testable per-address consecutive-notify-failure counter backing
 * [BlePeripheral]'s notify-failure tolerance -- same no-Android-deps pattern
 * as [ContactReachability] / [MeshRouterState]. See [BlePeripheral]'s
 * `onNotificationSent` for the live bug this exists to fix (Pixel 10 Pro
 * field log, 2026-07-17): `onNotificationSent` fired status=129
 * (GATT_CONGESTED-adjacent) 14 times in a row for the same address during
 * the on-HELLO frame spray, and the old code tore the link down on the very
 * first one -- wiping MeshRouter's learned address->userId mapping even
 * though the central kept writing to us on that exact address afterward.
 * Only a run of [maxConsecutiveFailures] notify failures for the SAME
 * address, with no success in between, is proof the link is actually gone
 * over this path. A real STATE_DISCONNECTED callback always tears the link
 * down immediately regardless of this count -- this class only governs the
 * notify-failure path.
 */
class NotifyFailureTracker(private val maxConsecutiveFailures: Int = MAX_CONSECUTIVE_FAILURES) {
    private val counts = mutableMapOf<String, Int>()

    /**
     * Records one notify failure for [address]. Returns true once [address]
     * has failed [maxConsecutiveFailures] times in a row with no
     * intervening success -- the caller's signal to actually tear the link
     * down.
     */
    fun recordFailure(address: String): Boolean {
        val count = (counts[address] ?: 0) + 1
        counts[address] = count
        return count >= maxConsecutiveFailures
    }

    /** A notify to [address] succeeded; its failure streak resets. */
    fun recordSuccess(address: String) {
        counts.remove(address)
    }

    /** [address] disconnected (or was torn down); forget its failure history. */
    fun clear(address: String) {
        counts.remove(address)
    }

    /** Forgets every address's failure history, e.g. on a full role stop(). */
    fun clearAll() {
        counts.clear()
    }

    /** Current consecutive-failure count for [address], for tests/diagnostics. */
    fun failureCount(address: String): Int = counts[address] ?: 0

    companion object {
        const val MAX_CONSECUTIVE_FAILURES = 3
    }
}
