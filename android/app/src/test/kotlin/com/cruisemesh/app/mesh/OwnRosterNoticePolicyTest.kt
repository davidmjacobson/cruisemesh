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
                provenOwnDeviceId = null,
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
                provenOwnDeviceId = null,
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
                provenOwnDeviceId = null,
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
                provenOwnDeviceId = null,
            ),
        )
    }

    /**
     * **The 2026-08-24 field case, pinned.**
     *
     * Two phones §9 linked as devices of one person hold *different* agreement
     * keys: the ceremony gives the new device its own and withholds the person
     * root secret. So the sibling's Noise static is not ours, and every
     * agreement-key comparison on this path answers "stranger" — which is what
     * refused the link 25 times across 15 minutes on one `/24` and left a
     * removed phone believing it was still linked.
     *
     * What admits it is the roster proof the transport already verified for this
     * session. Note the agreement keys deliberately do *not* match here: this
     * case fails on the pre-fix predicate for exactly the reason the field did.
     */
    @Test
    fun `a sibling that shares no agreement key may carry one`() {
        assertTrue(
            OwnRosterNoticePolicy.mayCross(
                isLanLink = true,
                ownAgreePk = OWN_AGREE_PK,
                sessionRemoteStaticKey = SOMEONE_ELSE,
                provenOwnDeviceId = SIBLING_DEVICE_ID,
            ),
        )
    }

    /**
     * §10 step 5's whole purpose: the device that most needs the notice is the
     * one that was removed. The transport admits a tombstoned device by design,
     * and this predicate must not undo that — refusing here would slam the only
     * door the notice can come through.
     */
    @Test
    fun `a removed sibling may still be told, which is the point`() {
        assertTrue(
            OwnRosterNoticePolicy.mayCross(
                isLanLink = true,
                ownAgreePk = OWN_AGREE_PK,
                sessionRemoteStaticKey = SOMEONE_ELSE,
                provenOwnDeviceId = REMOVED_DEVICE_ID,
            ),
        )
    }

    /**
     * A proven device id is still not a licence to skip the transport check: a
     * BLE link carries no Noise session and therefore proves nothing, so the
     * transport can never produce one there. Pinned so a future caller that
     * guesses at an id cannot open the BLE path by accident.
     */
    @Test
    fun `a BLE link may not, even holding a device id`() {
        assertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink = false,
                ownAgreePk = OWN_AGREE_PK,
                sessionRemoteStaticKey = null,
                provenOwnDeviceId = SIBLING_DEVICE_ID,
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

        /**
         * A device id the transport reports having verified for this session.
         * Sixteen bytes, as `core_derive_device_id` produces — the value itself
         * is opaque here, because this predicate's job is to distinguish
         * "proved" from "did not", not to re-verify what core already checked.
         */
        val SIBLING_DEVICE_ID = ByteArray(16) { 0x33 }
        val REMOVED_DEVICE_ID = ByteArray(16) { 0x44 }
    }
}
