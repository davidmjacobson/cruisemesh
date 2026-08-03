package com.cruisemesh.app.friending

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.ContactDiscoveryPolicy
import uniffi.cruisemesh_core.OutgoingSharedRequest
import uniffi.cruisemesh_core.SharedRequestDismissal

private const val DAY_MS = 24L * 60L * 60L * 1000L
private const val NOW = 1_700_000_000_000L

class ShareContactPolicyTest {

    private fun policy(enabled: Boolean, revision: ULong = 7uL) = ContactDiscoveryPolicy(
        userId = ByteArray(16) { 1 },
        protocolVersion = 2u,
        enabled = enabled,
        revision = revision,
    )

    private fun dismissal(count: UInt, suppressed: Boolean = false) = SharedRequestDismissal(
        requesterUserId = ByteArray(16) { 2 },
        count = count,
        suppressed = suppressed,
    )

    @Test
    fun `their switch governs, and an absent one is not permission`() {
        assertEquals(
            ShareContactAvailability.AVAILABLE,
            ShareContactPolicy.availability(policy(enabled = true), isBlocked = false),
        )
        assertEquals(
            ShareContactAvailability.DISCOVERY_OFF,
            ShareContactPolicy.availability(policy(enabled = false), isBlocked = false),
        )
        // A contact who never advertised a policy: unknown is not yes. Turning
        // it off already means "do not hand me around", and having never said
        // so is not a weaker statement than saying it.
        assertEquals(
            ShareContactAvailability.DISCOVERY_OFF,
            ShareContactPolicy.availability(null, isBlocked = false),
        )
    }

    @Test
    fun `a blocked contact offers nothing, not even an explanation of theirs`() {
        // Their switch is not the reason, and saying it would be misleading:
        // blocking stops our own participation (decision 9).
        assertEquals(
            ShareContactAvailability.HIDDEN,
            ShareContactPolicy.availability(policy(enabled = true), isBlocked = true),
        )
        assertEquals(
            ShareContactAvailability.HIDDEN,
            ShareContactPolicy.availability(null, isBlocked = true),
        )
    }

    @Test
    fun `waiting stops being true when the card dies`() {
        val request = OutgoingSharedRequest(
            candidateUserId = ByteArray(16) { 3 },
            expiresAtMs = NOW + DAY_MS,
            sentAtMs = NOW,
        )
        assertEquals(OutgoingSharedState.NONE, ShareContactPolicy.outgoingState(null, NOW))
        assertEquals(OutgoingSharedState.WAITING, ShareContactPolicy.outgoingState(request, NOW))
        // No answer ever comes for a rejected request, so past expiry the UI
        // must stop implying one might.
        assertEquals(
            OutgoingSharedState.NO_RESPONSE,
            ShareContactPolicy.outgoingState(request, NOW + DAY_MS),
        )
        assertEquals(
            OutgoingSharedState.NO_RESPONSE,
            ShareContactPolicy.outgoingState(request, NOW + 30 * DAY_MS),
        )
    }

    @Test
    fun `don't ask again appears on the second ask, not the first`() {
        assertFalse(ShareContactPolicy.offerSuppression(null))
        assertFalse(ShareContactPolicy.offerSuppression(dismissal(0u)))
        assertTrue(ShareContactPolicy.offerSuppression(dismissal(1u)))
        assertTrue(ShareContactPolicy.offerSuppression(dismissal(4u)))
    }

    @Test
    fun `an already-suppressed requester is not offered the exit again`() {
        val suppressed = dismissal(2u, suppressed = true)
        assertTrue(ShareContactPolicy.suppressed(suppressed))
        assertFalse(ShareContactPolicy.offerSuppression(suppressed))
        assertFalse(ShareContactPolicy.suppressed(dismissal(2u)))
        assertFalse(ShareContactPolicy.suppressed(null))
    }

    @Test
    fun `expiry is stated in whole days, rounded up and never zero`() {
        assertEquals(7, ShareContactPolicy.daysUntil(NOW + 7 * DAY_MS, NOW))
        // Part of a day left still buys the whole sentence "stops working in 1 day".
        assertEquals(1, ShareContactPolicy.daysUntil(NOW + 1, NOW))
        assertEquals(7, ShareContactPolicy.daysUntil(NOW + 7 * DAY_MS - 1, NOW))
        assertEquals(1, ShareContactPolicy.daysUntil(NOW - DAY_MS, NOW))
    }
}
