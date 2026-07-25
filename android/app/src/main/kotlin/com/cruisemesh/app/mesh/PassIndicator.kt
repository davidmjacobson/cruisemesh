package com.cruisemesh.app.mesh

/**
 * Severity of the Cruise Pass status shown in Settings, derived from
 * [RelayHealth] plus whether a setup card is saved at all.
 *
 * Deliberately coarser than [RelayHealth]: the row needs one glanceable
 * symbol, and several distinct health states call for the same reaction from
 * the person holding the phone.
 *
 * Pure Kotlin with no Android imports so the mapping is unit-testable
 * directly; the icon and tint for each value live in the Compose layer
 * (SettingsScreen), and iOS mirrors this in PassIndicator.swift.
 */
enum class PassIndicator {
    /**
     * Show nothing. Either no pass is set up -- which is the free default,
     * where nearby delivery still works and so must never be dressed up as a
     * fault -- or the first authenticated check has not finished yet, where a
     * symbol would only flicker.
     */
    NONE,

    /** Green check: the relay answered and the pass is good. */
    READY,

    /**
     * Neutral: the pass is fine, this phone just has no internet right now.
     *
     * Never an error. CruiseMesh exists for exactly this situation -- being
     * at sea with no connectivity is the normal case, not a failure -- and a
     * red mark here would be both wrong and a fast way to teach people to
     * ignore the indicator when it finally does mean something.
     */
    WAITING,

    /**
     * Amber: reached the relay, or tried to, and something is off in a way
     * that may clear on its own (service unavailable). Worth a glance, not
     * worth acting on yet.
     */
    ATTENTION,

    /**
     * Red: the pass will not work again until the person does something --
     * renew it, replace the setup card, or contact support. These states do
     * not self-heal, which is what separates them from [ATTENTION].
     */
    ACTION_REQUIRED,
}

/**
 * Map relay health to the Settings indicator. [configured] is whether a setup
 * card is saved at all, which [RelayHealth] alone cannot express: a phone
 * that has never had a pass and a phone whose pass is saved but unchecked can
 * both report [RelayHealth.NoConfig].
 */
fun passIndicator(health: RelayHealth, configured: Boolean): PassIndicator {
    if (!configured) return PassIndicator.NONE
    return when (health) {
        RelayHealth.NoConfig,
        RelayHealth.Checking,
        -> PassIndicator.NONE
        is RelayHealth.Ok -> PassIndicator.READY
        RelayHealth.NoInternet -> PassIndicator.WAITING
        is RelayHealth.Failing -> PassIndicator.ATTENTION
        is RelayHealth.Expired,
        is RelayHealth.Suspended,
        is RelayHealth.TokenRejected,
        -> PassIndicator.ACTION_REQUIRED
    }
}
