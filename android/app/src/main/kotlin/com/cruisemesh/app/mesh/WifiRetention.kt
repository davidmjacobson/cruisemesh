package com.cruisemesh.app.mesh

/**
 * T15 phase 2/3 decision logic, kept free of Android framework types so it can
 * be unit-tested. The framework plumbing (the actual `requestNetwork` hold and
 * the connectivity observations that feed these) lives in [MeshService] and
 * [WifiAssociationHold].
 *
 * The problem: on a ship (or any captive/internet-less Wi-Fi), Android's
 * adaptive connectivity tears down a Wi-Fi association that has no validated
 * internet and moves the phone to cellular. That kills the same-LAN transport
 * that reaches nearby phones fast. Holding an internet-less `TRANSPORT_WIFI`
 * request is the documented way to ask the OS to keep that association.
 */
object WifiHoldPolicy {
    /**
     * Whether to hold the internet-less Wi-Fi association right now.
     *
     * We hold it only when no VPN owns the default route. Under a full-tunnel
     * VPN (the test Pixel's always-on WireGuard) the Wi-Fi is already pinned as
     * the tunnel's underlying transport, so our own request would be redundant
     * at best and could confound that environment at worst -- so there we are a
     * deliberate no-op, matching the relay-bind logic's VPN handling.
     */
    fun shouldHold(isVpnDefault: Boolean): Boolean = !isVpnDefault
}

/**
 * Detects the "Wi-Fi quietly dropped out from under a live mesh" pattern so the
 * app can nudge the user to keep Wi-Fi on, and decides when that nudge has
 * earned its place on screen. Pure; [MeshService] feeds it real timestamps and
 * a persisted occurrence count.
 */
object WifiDropPolicy {
    /**
     * A Wi-Fi loss within this window of joining the mesh, while cellular
     * stayed up, reads as adaptive-connectivity dropping an internet-less
     * association rather than the user genuinely walking out of range.
     */
    const val PREMATURE_WINDOW_MS: Long = 3 * 60 * 1000

    /** Show the tip only after the pattern repeats, so a one-off can't nag. */
    const val TIP_THRESHOLD: Int = 2

    /**
     * True when a Wi-Fi loss looks like a premature adaptive-connectivity
     * teardown: it happened within [PREMATURE_WINDOW_MS] of the mesh coming up
     * and cellular was still available (so the phone didn't just lose all
     * connectivity).
     */
    fun isPrematureDrop(joinedAtMs: Long, lostAtMs: Long, cellularUp: Boolean): Boolean {
        if (!cellularUp) return false
        val elapsed = lostAtMs - joinedAtMs
        return elapsed in 0..PREMATURE_WINDOW_MS
    }

    /** Whether the contextual "keep Wi-Fi on" tip should be shown. */
    fun shouldShowTip(occurrences: Int): Boolean = occurrences >= TIP_THRESHOLD
}
