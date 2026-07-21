package com.cruisemesh.app.mesh

/** How much of the subnet an automatic sweep covers. */
internal enum class LanScanBreadth {
    /** Just this phone's /24 (or the actual subnet when it is narrower) -- ~1.5s of probes. */
    LOCAL_24,

    /** The network's whole advertised subnet, clamped to a /20 -- ~4,094 hosts of probes. */
    FULL_SUBNET,
}

/**
 * Pure, unit-testable schedule for the automatic subnet sweep, deciding which
 * [LanScanBreadth] (if any) is due whenever [LanTransport]'s periodic check
 * fires and finds the transport lonely (no connections, nothing in flight --
 * that gate stays [LanTransport]'s job, see `shouldRunAutomaticLanScan`).
 *
 * The full-subnet sweep is expensive (up to a /20, ~4,094 TCP probes at
 * concurrency 64) and ship/hotel Wi-Fi -- the app's core deployment -- is
 * exactly where the underlying network tends to be a huge flat subnet, so
 * it is deliberately hard to trigger:
 *
 *  - [LanScanBreadth.LOCAL_24] is cheap and keeps the flat [localIntervalMs]
 *    cadence. It also always runs before the first full sweep on a network --
 *    DHCP tends to cluster leases, so a peer that joined around the same time
 *    is disproportionately likely to be in our /24.
 *  - [LanScanBreadth.FULL_SUBNET] only ever becomes eligible after a /24
 *    sweep on this network join has completed and found *zero* peers
 *    ([onScanCompleted]'s `foundPeer`) -- a /24 sweep that finds a peer never
 *    arms it, since that peer is already proof discovery works here. Once
 *    eligible it waits a real delay ([emptyLocalSweepFullDelayMs], default
 *    60s) before firing, then backs off further ([fullBackoffMs]) each time
 *    it runs and still finds nobody. [onPeerEvidence] (an NSD resolution or
 *    an endpoint hint -- proof peers exist here) resets that backoff, but
 *    callers must only invoke it for genuinely NEW evidence: repeated
 *    evidence about an already-connected/linked peer (e.g. its Bonjour/NSD
 *    record refreshing) must not keep re-triggering sweeps.
 *
 * Methods are @Synchronized leaf-monitor style: callers are the main handler
 * plus scan worker threads (sweep completion).
 */
internal class LanScanPlanner(
    private val localIntervalMs: Long = LOCAL_SCAN_INTERVAL_MS,
    private val fullBackoffMs: List<Long> = FULL_SCAN_BACKOFF_MS,
    private val emptyLocalSweepFullDelayMs: Long = EMPTY_LOCAL_SWEEP_FULL_DELAY_MS,
) {
    private var joined = false
    private var localDueAtMs = 0L

    /** Armed only once a /24 sweep has completed on this network join and found nobody. */
    private var fullEligible = false
    private var fullDueAtMs = 0L
    private var fullBackoffIndex = 0

    /** A LAN session came up on a (new or rejoined) network: both tiers re-anchor to now. */
    @Synchronized
    fun onNetworkJoined(nowMs: Long) {
        joined = true
        localDueAtMs = nowMs
        fullEligible = false
        fullDueAtMs = 0L
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
     * full tier is never due before a /24 sweep has completed empty on this
     * network (see the class doc).
     */
    @Synchronized
    fun takeDueScan(nowMs: Long): LanScanBreadth? {
        if (!joined) return null
        if (nowMs >= localDueAtMs) {
            localDueAtMs = nowMs + localIntervalMs
            return LanScanBreadth.LOCAL_24
        }
        if (fullEligible && nowMs >= fullDueAtMs) {
            fullDueAtMs = nowMs + fullBackoffMs[fullBackoffIndex]
            if (fullBackoffIndex < fullBackoffMs.lastIndex) fullBackoffIndex++
            return LanScanBreadth.FULL_SUBNET
        }
        return null
    }

    /**
     * A sweep of [breadth] finished probing every candidate; [foundPeer]
     * reports whether any candidate answered. Only a /24 sweep that found
     * nobody arms the full tier for the first time -- a /24 sweep that finds
     * a peer, or one that runs after the tier is already armed, leaves the
     * existing full-sweep schedule untouched.
     */
    @Synchronized
    fun onScanCompleted(breadth: LanScanBreadth, nowMs: Long, foundPeer: Boolean) {
        if (breadth != LanScanBreadth.LOCAL_24) return
        if (!fullEligible && !foundPeer) {
            fullEligible = true
            fullDueAtMs = nowMs + emptyLocalSweepFullDelayMs
            fullBackoffIndex = 0
        }
    }

    /**
     * Evidence a peer is on this network right now (NSD resolved a CruiseMesh
     * service, or a contact's endpoint hint arrived): a full sweep is worth
     * retrying promptly if the direct connection doesn't pan out. Only
     * meaningful once the full tier is already eligible ([onScanCompleted]) --
     * before that, evidence doesn't change anything, since the full sweep
     * isn't on the table yet. Callers are responsible for only calling this
     * for genuinely NEW evidence (see the class doc).
     */
    @Synchronized
    fun onPeerEvidence(nowMs: Long) {
        if (!joined || !fullEligible) return
        fullBackoffIndex = 0
        fullDueAtMs = minOf(fullDueAtMs, nowMs)
    }

    /**
     * A broad-enough sweep received no TCP response at all, which commonly
     * means Wi-Fi client isolation. Defer further expensive full sweeps to the
     * backoff cap until fresh peer evidence or a network join resets the plan.
     */
    @Synchronized
    fun onIsolationSuspected(nowMs: Long) {
        if (!joined) return
        fullBackoffIndex = fullBackoffMs.lastIndex
        fullDueAtMs = nowMs + fullBackoffMs.last()
    }

    companion object {
        const val LOCAL_SCAN_INTERVAL_MS = 5 * 60_000L
        val FULL_SCAN_BACKOFF_MS = listOf(15 * 60_000L, 60 * 60_000L, 4 * 60 * 60_000L)

        // Delay before the full sweep first becomes due once an empty /24
        // sweep arms it. Deliberately not "a couple of seconds": there is no
        // rush to fire the expensive tier the instant the cheap one comes
        // back clean.
        const val EMPTY_LOCAL_SWEEP_FULL_DELAY_MS = 60_000L
    }
}
