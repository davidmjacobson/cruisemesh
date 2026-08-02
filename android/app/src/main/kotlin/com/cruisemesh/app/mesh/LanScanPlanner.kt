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
 *    sweep on this network join has completed and authenticated *zero*
 *    friends ([onScanCompleted]'s `foundPeer`) -- a sweep that produced an
 *    authenticated friend never arms it, since that friend is proof
 *    discovery works here. A bare TCP connect deliberately does not count:
 *    an unrelated service (or a stranger's CruiseMesh) on the default port
 *    must not disarm the wider sweep. Once
 *    eligible it waits a real delay ([emptyLocalSweepFullDelayMs], default
 *    60s) before firing, then backs off further ([fullBackoffMs]) each time
 *    it runs and still finds nobody. [onPeerEvidence] (an NSD resolution or
 *    an endpoint hint -- proof peers exist here) resets that backoff, but
 *    callers must only invoke it for genuinely NEW evidence: repeated
 *    evidence about an already-connected/linked peer (e.g. its Bonjour/NSD
 *    record refreshing) must not keep re-triggering sweeps. Evidence is also
 *    only trusted [maxPeerEvidenceResets] times per network join: the
 *    "genuinely new" test is a token another device on the Wi-Fi chooses, so
 *    an unbounded reset budget would let anything on a shared network keep
 *    every phone in range sweeping back to back. Past the budget the
 *    evidence still drives ordinary discovery and connection attempts --
 *    it just stops rewinding the sweep schedule.
 *
 * Methods are @Synchronized leaf-monitor style: callers are the main handler
 * plus scan worker threads (sweep completion).
 */
internal class LanScanPlanner(
    private val localIntervalMs: Long = LOCAL_SCAN_INTERVAL_MS,
    private val fullBackoffMs: List<Long> = FULL_SCAN_BACKOFF_MS,
    private val emptyLocalSweepFullDelayMs: Long = EMPTY_LOCAL_SWEEP_FULL_DELAY_MS,
    private val maxPeerEvidenceResets: Int = MAX_PEER_EVIDENCE_RESETS,
) {
    private var joined = false
    private var localDueAtMs = 0L

    /** Armed only once a /24 sweep has completed on this network join and found nobody. */
    private var fullEligible = false
    private var fullDueAtMs = 0L
    private var fullBackoffIndex = 0

    /** How much of this network join's [maxPeerEvidenceResets] budget is spent. */
    private var peerEvidenceResets = 0

    /** A LAN session came up on a (new or rejoined) network: both tiers re-anchor to now. */
    @Synchronized
    fun onNetworkJoined(nowMs: Long) {
        joined = true
        localDueAtMs = nowMs
        fullEligible = false
        fullDueAtMs = 0L
        fullBackoffIndex = 0
        peerEvidenceResets = 0
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
     * reports whether the sweep authenticated an accepted friend (not
     * merely whether some TCP service answered). Only a /24 sweep that
     * authenticated nobody arms the full tier for the first time -- one
     * that found a friend, or one that runs after the tier is already
     * armed, leaves the existing full-sweep schedule untouched.
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
     * retrying promptly if the direct connection doesn't pan out. Callers are
     * responsible for only calling this for genuinely NEW evidence (see the
     * class doc), and it is trusted at most [maxPeerEvidenceResets] times per
     * network join.
     *
     * Returns whether this evidence changed the schedule, so the caller knows
     * whether to bring its own next scan check forward. False once the budget
     * is spent, and false before the full tier is eligible ([onScanCompleted])
     * -- evidence can't conjure a full sweep out of nowhere, so there is
     * nothing to hurry towards yet.
     */
    @Synchronized
    fun onPeerEvidence(nowMs: Long): Boolean {
        if (!joined || !fullEligible) return false
        if (peerEvidenceResets >= maxPeerEvidenceResets) return false
        peerEvidenceResets++
        fullBackoffIndex = 0
        fullDueAtMs = minOf(fullDueAtMs, nowMs)
        return true
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

        // How many times peer evidence may rewind the full-sweep schedule on
        // one network join. Matched to the transport's simultaneous-link
        // ceiling (8): a whole family fleet announcing itself on arrival
        // still gets a prompt sweep each time, while anything else on the
        // Wi-Fi runs out of budget long before the expensive tier can be
        // driven back to back.
        const val MAX_PEER_EVIDENCE_RESETS = 8
    }
}
