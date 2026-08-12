package com.cruisemesh.app.mesh

/**
 * Severity of the Shore Pass status shown in Settings, derived from
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
     * Amber "?": something is off in a way that clears on its own -- the
     * service couldn't be reached just now, or asked us to slow down (429).
     * Worth a glance, never worth acting on, and never a reason to contact
     * anyone.
     */
    ATTENTION,

    /**
     * Red "!": internet delivery stays affected until the person does
     * something -- renew the pass, replace the setup card, send a smaller
     * message, or contact support. These states do not self-heal, which is
     * what separates them from [ATTENTION].
     */
    ACTION_REQUIRED,
}

/**
 * True when this health is an actual verdict on the pass rather than "we have
 * not looked yet". [RelayHealth.Checking] and [RelayHealth.NoConfig] both mean
 * the absence of an answer -- the latter is what
 * [MeshConnectivityStatus.clear] leaves behind when the service restarts, and
 * what a saved-but-unchecked card reports.
 */
fun RelayHealth.isPassVerdict(): Boolean =
    this !is RelayHealth.Checking && this !is RelayHealth.NoConfig

/**
 * Heading shown at the top of the Shore Pass screen.
 *
 * Pure so the flicker rules below are unit-testable; the copy lives in
 * `strings.xml` and iOS mirrors this in `PassIndicator.swift`.
 */
enum class ShorePassHeading {
    /** No setup card saved: invite them to add one. */
    NOT_SET_UP,

    /** A card is saved but no check has landed yet. */
    CHECKING,

    /** Green check: the relay answered and the pass is good. */
    READY,

    /** A card is saved and the last check said something other than OK. */
    CONFIGURED,
}

/**
 * Heading for a saved pass, given the live [health] and [lastVerdict] -- the
 * most recent health that was an actual answer (see [isPassVerdict]).
 *
 * Re-checks are not demotions. A background sync pass, or a service restart,
 * drops health to [RelayHealth.Checking]/[RelayHealth.NoConfig] for a second
 * or two; without [lastVerdict] the heading would fall from "Shore Pass is
 * set up" with its green check to "Shore Pass is configured" and back, which
 * reads to the person holding the phone as the pass breaking and healing.
 * This is the same reasoning that maps those two states to
 * [PassIndicator.NONE] rather than to a symbol that would only flicker.
 *
 * It stays health-only for every real verdict: the moment the relay answers
 * with anything but OK -- rejected token, expired, no internet -- the green
 * check goes, because [lastVerdict] is then that answer and not the stale OK.
 */
fun shorePassHeading(
    health: RelayHealth,
    configured: Boolean,
    lastVerdict: RelayHealth?,
): ShorePassHeading {
    if (!configured) return ShorePassHeading.NOT_SET_UP
    val settled = if (health.isPassVerdict()) health else lastVerdict ?: return ShorePassHeading.CHECKING
    return if (settled is RelayHealth.Ok) ShorePassHeading.READY else ShorePassHeading.CONFIGURED
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
        // A roaming deferral reads exactly like being offline: the pass is
        // healthy and the wait is deliberate, so it is never a fault state.
        RelayHealth.NoInternet,
        RelayHealth.DeferredRoaming,
        -> PassIndicator.WAITING
        // Transient, self-healing ("?"): can't reach right now, or told to
        // slow down. Same reaction either way -- none.
        is RelayHealth.Failing,
        is RelayHealth.RateLimited,
        -> PassIndicator.ATTENTION
        // Persistent, actionable ("!"): these stay until someone acts.
        is RelayHealth.Expired,
        is RelayHealth.Suspended,
        is RelayHealth.TokenRejected,
        is RelayHealth.QuotaFull,
        is RelayHealth.MessageTooLarge,
        -> PassIndicator.ACTION_REQUIRED
    }
}
