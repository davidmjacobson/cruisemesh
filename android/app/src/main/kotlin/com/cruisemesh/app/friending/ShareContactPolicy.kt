package com.cruisemesh.app.friending

import uniffi.cruisemesh_core.ContactDiscoveryPolicy
import uniffi.cruisemesh_core.OutgoingSharedRequest
import uniffi.cruisemesh_core.SharedRequestDismissal

private const val DAY_MS = 24L * 60L * 60L * 1000L

/**
 * Whether **Share contact** is offered for one contact (specs/share-contact.md
 * decisions 3 and 4), and when it is not, whether to say why.
 */
enum class ShareContactAvailability {
    /** Nothing about sharing belongs on this contact's screen. */
    HIDDEN,
    AVAILABLE,

    /** Their switch is off (or they never advertised one), so say so plainly. */
    DISCOVERY_OFF,
}

/** What the requester's own phone can honestly say about a request it sent. */
enum class OutgoingSharedState { NONE, WAITING, NO_RESPONSE }

/**
 * The share-contact decisions that are pure functions of stored state, kept out
 * of the composables so they can be unit-tested without an Android runtime.
 */
object ShareContactPolicy {

    /**
     * Decision 4: the shared person's own **Friends of friends** switch governs
     * manual introductions too, so an absent policy (a contact who never
     * advertised one) counts as off rather than as permission.
     */
    fun availability(
        policy: ContactDiscoveryPolicy?,
        isBlocked: Boolean,
    ): ShareContactAvailability = when {
        isBlocked -> ShareContactAvailability.HIDDEN
        policy?.enabled == true -> ShareContactAvailability.AVAILABLE
        else -> ShareContactAvailability.DISCOVERY_OFF
    }

    /**
     * Every rejection path on the other phone drops silently and **Not now**
     * sends nothing back, so past the card's expiry "waiting" stops being true
     * and the UI has to offer something actionable instead.
     */
    fun outgoingState(request: OutgoingSharedRequest?, nowMs: Long): OutgoingSharedState = when {
        request == null -> OutgoingSharedState.NONE
        nowMs >= request.expiresAtMs -> OutgoingSharedState.NO_RESPONSE
        else -> OutgoingSharedState.WAITING
    }

    /** Suppressed requesters never reach a prompt at all. */
    fun suppressed(dismissal: SharedRequestDismissal?): Boolean = dismissal?.suppressed == true

    /**
     * **Don't ask again** appears from the *second* ask onward: one dismissal
     * already on file means this sheet is the repeat.
     */
    fun offerSuppression(dismissal: SharedRequestDismissal?): Boolean =
        !suppressed(dismissal) && (dismissal?.count ?: 0u) >= 1u

    /** Whole days left on a card, rounded up and never below one. */
    fun daysUntil(expiresAtMs: Long, nowMs: Long): Int {
        val remaining = expiresAtMs - nowMs
        if (remaining <= 0L) return 1
        return ((remaining + DAY_MS - 1L) / DAY_MS).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
    }
}
