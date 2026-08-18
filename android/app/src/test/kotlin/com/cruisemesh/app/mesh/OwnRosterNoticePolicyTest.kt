package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.coreOwnCapabilities

/**
 * Who may be handed this person's own device list (§10 step 5).
 *
 * The frame is plaintext on the link, so this predicate is the whole of its
 * safety — a stranger who merely *claims* our user id in a HELLO must not be
 * able to ask how many devices we have, and must not be able to hand us a
 * document to act on. Both directions run through [OwnRosterNoticePolicy.mayCross],
 * so both are pinned here.
 */
class OwnRosterNoticePolicyTest {

    @Test
    fun `a link that proved it holds our own key may carry one`() {
        assertTrue(
            OwnRosterNoticePolicy.mayCross(
                isLanLink = true,
                ownAgreePk = OWN_AGREE_PK,
                sessionRemoteStaticKey = OWN_AGREE_PK.copyOf(),
            ),
        )
    }

    @Test
    fun `a stranger on the LAN may not, however its HELLO is addressed`() {
        assertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink = true,
                ownAgreePk = OWN_AGREE_PK,
                sessionRemoteStaticKey = SOMEONE_ELSE,
            ),
        )
    }

    @Test
    fun `an unauthenticated LAN link may not`() {
        assertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink = true,
                ownAgreePk = OWN_AGREE_PK,
                sessionRemoteStaticKey = null,
            ),
        )
    }

    /**
     * The BLE limitation, stated as a test rather than left to be rediscovered:
     * a BLE HELLO is cleartext and carries no proof at all, so a removed device
     * that only ever meets its fleet over Bluetooth does not converge. The spec
     * records it; weakening this line is not the fix.
     */
    @Test
    fun `a BLE link never may, because it proves nothing`() {
        assertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink = false,
                ownAgreePk = OWN_AGREE_PK,
                sessionRemoteStaticKey = OWN_AGREE_PK.copyOf(),
            ),
        )
    }

    @Test
    fun `a peer that never advertised the frame is not sent one`() {
        assertFalse(OwnRosterNoticePolicy.peerReadsNotices(0u))
        assertTrue(OwnRosterNoticePolicy.peerReadsNotices(OwnRosterNoticePolicy.CAPABILITY_BIT))
    }

    /**
     * The bit is a number in two places -- `CAP_OWN_ROSTER_NOTICE` in core and
     * the copy this shell tests peers against -- because a capability mask does
     * not cross the binding. This is the tripwire that stops the copy drifting:
     * our own advertisement is built by core, so if the bit moved there and not
     * here, this fails.
     */
    @Test
    fun `the bit this shell looks for is the one core advertises`() {
        assertEquals(1u shl 4, OwnRosterNoticePolicy.CAPABILITY_BIT)
        assertTrue(OwnRosterNoticePolicy.peerReadsNotices(coreOwnCapabilities()))
    }

    private companion object {
        val OWN_AGREE_PK = ByteArray(32) { 0x11 }
        val SOMEONE_ELSE = ByteArray(32) { 0x22 }
    }
}
