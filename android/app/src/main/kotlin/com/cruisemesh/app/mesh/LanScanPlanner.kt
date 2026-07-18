package com.cruisemesh.app.mesh

/** How much of the subnet an automatic sweep covers. */
internal enum class LanScanBreadth {
    /** Just this phone's /24 (or the actual subnet when it is narrower) -- ~1.5s of probes. */
    LOCAL_24,

    /** The network's whole advertised subnet, clamped to a /16 -- minutes of probes. */
    FULL_SUBNET,
}

/**
 * Pure, unit-testable schedule for the automatic subnet sweep, deciding which
 * [LanScanBreadth] (if any) is due whenever [LanTransport]'s periodic check
 * fires and finds the transport lonely (no connections, nothing in flight --
 * that gate stays [LanTransport]'s job, see `shouldRunAutomaticLanScan`).
 *
 * The /16 sweep is ~65k TCP probes: repeated on the old flat 5-minute cadence
 * it would run essentially back-to-back, which both drains battery and looks
 * like a port scan to ship network security gear. So the tiers differ:
 *
 *  - [LanScanBreadth.LOCAL_24] is cheap and keeps the flat [localIntervalMs]
 *    cadence. It also always runs before the first full sweep on a network --
 *    DHCP tends to cluster leases, so a peer that joined around the same time
 *    is disproportionately likely to be in our /24.
 *  - [LanScanBreadth.FULL_SUBNET] is anchored to joining the network (which is
 *    also when daily cruise-ship IP churn invalidates cached endpoints): one
 *    sweep shortly after join, then exponential backoff [fullBackoffMs] while
 *    still lonely. [onPeerEvidence] (an NSD resolution or an endpoint hint --
 *    proof peers exist here) resets the backoff so a failed direct connection
 *    gets a prompt sweep behind it.
 *
 * Methods are @Synchronized leaf-monitor style: callers are the main handler
 * plus scan worker threads (sweep completion).
 */
internal class LanScanPlanner(
    private val localIntervalMs: Long = LOCAL_SCAN_INTERVAL_MS,
    private val fullBackoffMs: List<Long> = FULL_SCAN_BACKOFF_MS,
) {
    private var joined = false
    private var localDueAtMs = 0L
    private var localCompletedSinceJoin = false
    private var fullDueAtMs = 0L
    private var fullBackoffIndex = 0

    /** A LAN session came up on a (new or rejoined) network: both tiers re-anchor to now. */
    @Synchronized
    fun onNetworkJoined(nowMs: Long) {
        joined = true
        localDueAtMs = nowMs
        localCompletedSinceJoin = false
        fullDueAtMs = nowMs
        fullBackoffIndex = 0
    }

    /** The LAN session tore down; nothing is due until the next [onNetworkJoined]. */
    @Synchronized
    fun onNetworkLost() {
        joined = false
    }

    /**
     * Claims the scan tier that is due at [nowMs], advancing its schedule, or
     * returns null when neither is. The local tier wins when both are due; the
     * full tier is never due before a local sweep has completed on this
     * network (completing while still lonely is what justifies escalating).
     */
    @Synchronized
    fun takeDueScan(nowMs: Long): LanScanBreadth? {
        if (!joined) return null
        if (nowMs >= localDueAtMs) {
            localDueAtMs = nowMs + localIntervalMs
            return LanScanBreadth.LOCAL_24
        }
        if (localCompletedSinceJoin && nowMs >= fullDueAtMs) {
            fullDueAtMs = nowMs + fullBackoffMs[fullBackoffIndex]
            if (fullBackoffIndex < fullBackoffMs.lastIndex) fullBackoffIndex++
            return LanScanBreadth.FULL_SUBNET
        }
        return null
    }

    /** A sweep of [breadth] finished probing every candidate. */
    @Synchronized
    fun onScanCompleted(breadth: LanScanBreadth) {
        if (breadth == LanScanBreadth.LOCAL_24) {
            localCompletedSinceJoin = true
        }
    }

    /**
     * Evidence a peer is on this network right now (NSD resolved a CruiseMesh
     * service, or a contact's endpoint hint arrived): a full sweep is worth
     * retrying promptly if the direct connection doesn't pan out.
     */
    @Synchronized
    fun onPeerEvidence(nowMs: Long) {
        fullBackoffIndex = 0
        if (joined) {
            fullDueAtMs = minOf(fullDueAtMs, nowMs)
        }
    }

    companion object {
        const val LOCAL_SCAN_INTERVAL_MS = 5 * 60_000L
        val FULL_SCAN_BACKOFF_MS = listOf(15 * 60_000L, 60 * 60_000L, 4 * 60 * 60_000L)
    }
}
