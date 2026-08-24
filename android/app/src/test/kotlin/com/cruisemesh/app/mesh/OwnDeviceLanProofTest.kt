package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreLanOwnDeviceProof
import uniffi.cruisemesh_core.Roster
import uniffi.cruisemesh_core.coreLinkGenesisRoster
import uniffi.cruisemesh_core.coreLinkSignNewDeviceRoster
import uniffi.cruisemesh_core.coreMintInboxKey
import uniffi.cruisemesh_core.coreOwnDeviceLanProof
import uniffi.cruisemesh_core.coreOwnDeviceLanProofOpen
import uniffi.cruisemesh_core.coreRevokeDevicesRoster
import uniffi.cruisemesh_core.generateDeviceKeypair
import uniffi.cruisemesh_core.generateIdentity

/**
 * **The 2026-08-24 field failure, reproduced through the bindings this shell
 * actually calls** (`specs/multi-device-v1.md` §10 step 5).
 *
 * Core pins the predicate in Rust. What this adds is the shell's own view: the
 * two phones are built through the shipped §9 ceremony rather than from fixtures,
 * so the fact the whole bug rests on — that two genuine siblings of one person
 * share **no** private key — is asserted here rather than assumed, and the
 * marshalling that carries a proof across the FFI is exercised.
 *
 * The failure this closes: `LanTransport.acceptOwnDeviceOrRefuse` used to ask
 * whether the peer's Noise static key *was this identity's own agreement key*.
 * That is a clone test. §9's ceremony gives a linked device keys of its own and
 * withholds the person root secret, so it refused every real sibling, in both
 * roles — 25 refusals across 15 minutes on one `/24`, no own-device link ever
 * formed, and a removed phone that never learned it was removed.
 */
class OwnDeviceLanProofTest {

    /**
     * Two devices of one person, built the way §9 builds them, plus the rosters
     * each side actually holds after a removal.
     */
    private class Fleet {
        val person = generateIdentity()

        /** The approving phone -- the P10P in the field capture. */
        val approver = generateDeviceKeypair()

        /** The phone that gets removed -- the P7, which stayed wedged. */
        val sibling = generateDeviceKeypair()

        /** Both devices live: what each side held before the removal. */
        val bothLive: Roster

        /**
         * The approver's roster after removing the sibling: the sibling out of
         * `devices`, buried in `tombstones`, and the inbox key generation
         * climbed by §10.1's rotation.
         */
        val afterRemoval: Roster

        init {
            val genesis = coreLinkGenesisRoster(
                person.signSk,
                approver.signPk,
                approver.agreePk,
            )
            bothLive = coreLinkSignNewDeviceRoster(
                genesis,
                person.signPk,
                approver.signSk,
                sibling.signPk,
                sibling.agreePk,
            ).roster
            afterRemoval = coreRevokeDevicesRoster(
                bothLive,
                person.signPk,
                approver.signSk,
                listOf(sibling.deviceId),
                coreMintInboxKey(bothLive.inboxKeyGeneration),
            ).roster
        }
    }

    /** Stands in for a Noise transcript hash; two distinct values are two sessions. */
    private fun transcript(byte: Int) = ByteArray(32) { byte.toByte() }

    /**
     * **The fact the bug rested on.** Nothing in the ceremony gives the new
     * device the person's agreement key, so the predicate that compared them
     * could never have admitted a sibling — before any removal, and independent
     * of address family.
     */
    @Test
    fun `two devices of one person share no agreement key`() {
        val fleet = Fleet()
        assertNotEquals(
            fleet.approver.agreePk.toList(),
            fleet.sibling.agreePk.toList(),
        )
        // And a linked phone's shell identity is its own too, which is why its
        // HELLO carried a user id its sibling did not recognise.
        val linkedShellIdentity = generateIdentity()
        assertNotEquals(
            linkedShellIdentity.agreePk.toList(),
            fleet.person.agreePk.toList(),
        )
        assertNotEquals(
            linkedShellIdentity.userId.toList(),
            fleet.person.userId.toList(),
        )
    }

    /** The ordinary sibling meeting, both directions, before anything is removed. */
    @Test
    fun `each sibling admits the other on a roster proof`() {
        val fleet = Fleet()
        val session = transcript(0xA1)

        assertEquals(
            fleet.sibling.deviceId.toList(),
            coreOwnDeviceLanProofOpen(
                fleet.bothLive,
                session,
                coreOwnDeviceLanProof(fleet.sibling.signSk, session),
            ).deviceIdOrFail().toList(),
        )
        assertEquals(
            fleet.approver.deviceId.toList(),
            coreOwnDeviceLanProofOpen(
                fleet.bothLive,
                session,
                coreOwnDeviceLanProof(fleet.approver.signSk, session),
            ).deviceIdOrFail().toList(),
        )
    }

    /**
     * **§10 step 5's actual job: the exact post-removal state of BOTH sides.**
     *
     * The approver holds the burying roster — sibling tombstoned, inbox key
     * generation climbed. The removed phone still holds the pre-removal one,
     * both devices, older generation, because nothing has told it yet; that is
     * the whole point. Each must admit the other, or the notice has no link to
     * cross and the removed phone stays wedged, which is precisely what the
     * field capture recorded.
     */
    @Test
    fun `both sides of a removal still admit each other, so the notice can cross`() {
        val fleet = Fleet()
        val session = transcript(0xB2)
        assertTrue(
            "the removal must have rotated the inbox key generation",
            fleet.afterRemoval.inboxKeyGeneration > fleet.bothLive.inboxKeyGeneration,
        )

        // The approver, verifying the phone it removed: admitted, and told which
        // case it admitted. Refusing here would slam the only door the notice
        // can come through.
        val fromRemoved = coreOwnDeviceLanProofOpen(
            fleet.afterRemoval,
            session,
            coreOwnDeviceLanProof(fleet.sibling.signSk, session),
        )
        assertEquals(fleet.sibling.deviceId.toList(), fromRemoved.deviceIdOrFail().toList())
        assertTrue("the approver must know this is the removed device", fromRemoved!!.revoked)

        // The removed phone, verifying the approver against the stale roster it
        // still believes.
        val fromApprover = coreOwnDeviceLanProofOpen(
            fleet.bothLive,
            session,
            coreOwnDeviceLanProof(fleet.approver.signSk, session),
        )
        assertEquals(fleet.approver.deviceId.toList(), fromApprover.deviceIdOrFail().toList())
        assertTrue("the approver is not revoked", !fromApprover!!.revoked)
    }

    /**
     * The link the notice may cross is exactly the link the proof opened. Read
     * together with [OwnRosterNoticePolicyTest]: core says who proved it, and
     * the policy says a proven link may carry the roster even though the two
     * agreement keys will never match.
     */
    @Test
    fun `a proven sibling link may carry the notice both before and after removal`() {
        val fleet = Fleet()
        val session = transcript(0xC3)
        val proven = coreOwnDeviceLanProofOpen(
            fleet.afterRemoval,
            session,
            coreOwnDeviceLanProof(fleet.sibling.signSk, session),
        ).deviceIdOrFail()

        assertTrue(
            OwnRosterNoticePolicy.mayCross(
                isLanLink = true,
                ownAgreePk = fleet.approver.agreePk,
                // Not ours, and never will be. This is the field's exact input.
                sessionRemoteStaticKey = fleet.sibling.agreePk,
                provenOwnDeviceId = proven,
            ),
        )
    }

    /** A stranger on the same Wi-Fi produces nothing this roster will accept. */
    @Test
    fun `a device this person's roster never named is refused`() {
        val fleet = Fleet()
        val session = transcript(0xD4)
        val stranger = generateDeviceKeypair()
        assertNull(
            coreOwnDeviceLanProofOpen(
                fleet.bothLive,
                session,
                coreOwnDeviceLanProof(stranger.signSk, session),
            ),
        )
        assertTrue(
            !OwnRosterNoticePolicy.mayCross(
                isLanLink = true,
                ownAgreePk = fleet.approver.agreePk,
                sessionRemoteStaticKey = stranger.agreePk,
                provenOwnDeviceId = null,
            ),
        )
    }

    /**
     * A proof is worthless on any session but the one whose transcript it
     * names, so a recording off the wire cannot be replayed and a machine in the
     * middle cannot forward one.
     */
    @Test
    fun `a proof recorded from one session does not open on another`() {
        val fleet = Fleet()
        val recorded = coreOwnDeviceLanProof(fleet.sibling.signSk, transcript(0xE5))
        assertNull(coreOwnDeviceLanProofOpen(fleet.bothLive, transcript(0xE6), recorded))
    }

    @Test
    fun `a truncated or padded proof is refused rather than interpreted`() {
        val fleet = Fleet()
        val session = transcript(0xF7)
        val good = coreOwnDeviceLanProof(fleet.sibling.signSk, session)
        for (spoiled in listOf(
            ByteArray(0),
            good.copyOf(good.size - 1),
            good + ByteArray(1),
            good.copyOf().also { it[0] = (it[0].toInt() xor 0xff).toByte() },
            good.copyOf().also { it[it.size - 1] = (it[it.size - 1].toInt() xor 1).toByte() },
        )) {
            assertNull(coreOwnDeviceLanProofOpen(fleet.bothLive, session, spoiled))
        }
    }

    private fun CoreLanOwnDeviceProof?.deviceIdOrFail(): ByteArray =
        this?.deviceId ?: throw AssertionError("the roster should have named this device")
}
