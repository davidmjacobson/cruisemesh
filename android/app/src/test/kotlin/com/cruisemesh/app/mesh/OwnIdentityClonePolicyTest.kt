package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.CoreLanProofRole
import uniffi.cruisemesh_core.DeviceKeypair
import uniffi.cruisemesh_core.OwnDeviceFleet
import uniffi.cruisemesh_core.Roster
import uniffi.cruisemesh_core.RosterVersion
import uniffi.cruisemesh_core.coreLinkGenesisRoster
import uniffi.cruisemesh_core.coreLinkSignNewDeviceRoster
import uniffi.cruisemesh_core.coreOwnDeviceLanProof
import uniffi.cruisemesh_core.coreOwnDeviceLanProofOpen
import uniffi.cruisemesh_core.generateDeviceKeypair
import uniffi.cruisemesh_core.generateIdentity

/**
 * The clone guard's decision, driven with the same inputs the LAN transport
 * hands it (`specs/multi-device-v1.md` §1, §6, §10 step 5).
 *
 * The `provenPeerDeviceId` argument is never invented here. Every case mints a
 * real §10 step 5 proof and opens it through
 * [uniffi.cruisemesh_core.coreOwnDeviceLanProofOpen], so "no proof", "a proof
 * that does not verify" and "a proof from a device this roster never named" are
 * three distinct journeys to the same null rather than three spellings of one
 * literal — which is the difference between testing the guard and testing the
 * test.
 *
 * What the guard rests on, and what it does not: a proof says the far end holds
 * the secret half of a device signing key this person's roster names, on this
 * session. It rules out replay (the transcript is unique to one handshake and
 * the signed bytes name which end minted them) and it rules out a `.cmbak`
 * restore, which carries the person identity but no device signing secret. It
 * says nothing about distinct hardware, and a device whose signing secret was
 * extracted would still clear it.
 *
 * One thing these cases do NOT claim: that a sibling is recognised on today's
 * LAN. §10 step 5 keeps the clone arm of the handshake symmetric and proof-free,
 * so the arm that fires this guard always hands it null and the answer is always
 * [OwnIdentityClonePolicy.Verdict.CLONE]. What is pinned here is the rule, and
 * that both shells now compute it from the same two facts instead of one shell
 * hardcoding a null and the other never asking.
 */
class OwnIdentityClonePolicyTest {

    /** Two devices of one person, built the way §9's ceremony builds them. */
    private class Fleet {
        val person = generateIdentity()
        val thisPhone = generateDeviceKeypair()
        val sibling = generateDeviceKeypair()

        /** The roster this phone holds once both devices are linked. */
        val roster: Roster

        init {
            val genesis = coreLinkGenesisRoster(
                person.signSk,
                thisPhone.signPk,
                thisPhone.agreePk,
            )
            roster = coreLinkSignNewDeviceRoster(
                genesis,
                person.signPk,
                thisPhone.signSk,
                sibling.signPk,
                sibling.agreePk,
            ).roster
        }

        /** What core projects for this phone out of that roster. */
        fun fleetRecord() = OwnDeviceFleet(
            ownDeviceId = thisPhone.deviceId,
            deviceIds = listOf(thisPhone.deviceId, sibling.deviceId),
            projectedFrom = RosterVersion(recoveryEpoch = 0uL, seq = roster.seq),
        )
    }

    /** Stands in for a Noise transcript hash; two values are two sessions. */
    private fun transcript(byte: Int) = ByteArray(32) { byte.toByte() }

    /**
     * What this phone would learn from a peer's proof on one session — exactly
     * the value [LanTransport] passes the guard.
     */
    private fun opened(
        fleet: Fleet,
        payload: ByteArray?,
        session: ByteArray,
        peerRole: CoreLanProofRole = CoreLanProofRole.INITIATOR,
    ): ByteArray? = payload?.let {
        coreOwnDeviceLanProofOpen(
            fleet.roster,
            session,
            it,
            peerRole,
            fleet.thisPhone.deviceId,
        )?.deviceId
    }

    private fun proof(
        device: DeviceKeypair,
        session: ByteArray,
        role: CoreLanProofRole = CoreLanProofRole.INITIATOR,
    ): ByteArray = coreOwnDeviceLanProof(device.signSk, session, role)

    /**
     * The verdict for a peer whose Noise static key *is* this identity's own
     * agreement key — the only peers the guard has an opinion about.
     */
    private fun verdictForPeerHoldingOurKey(
        fleet: Fleet,
        provenPeerDeviceId: ByteArray?,
        projection: () -> OwnDeviceFleet? = { fleet.fleetRecord() },
    ) = OwnIdentityClonePolicy.verdict(
        ownAgreePk = fleet.person.agreePk,
        remoteStaticKey = fleet.person.agreePk.copyOf(),
        fleet = projection,
        provenPeerDeviceId = provenPeerDeviceId,
    )

    /**
     * **The case the whole change exists for.** A device this person's own
     * roster names, which proved it on this session, is not a clone — however
     * the key test reads.
     */
    @Test
    fun `a sibling that proved itself is not flagged`() {
        val fleet = Fleet()
        val session = transcript(0xA1)
        val proven = opened(fleet, proof(fleet.sibling, session), session)
        assertEquals(
            "the sibling's own proof must open against this phone's roster",
            fleet.sibling.deviceId.toList(),
            proven?.toList(),
        )
        assertEquals(
            OwnIdentityClonePolicy.Verdict.SIBLING,
            verdictForPeerHoldingOurKey(fleet, proven),
        )
    }

    /**
     * A peer holding this person's identity that produced no proof at all. The
     * `.cmbak` restore of §1 is exactly this: it carries the person identity and
     * the message store, and no device signing secret, so it has nothing the
     * roster could name.
     */
    @Test
    fun `a peer that proved nothing is flagged`() {
        val fleet = Fleet()
        assertEquals(
            OwnIdentityClonePolicy.Verdict.CLONE,
            verdictForPeerHoldingOurKey(fleet, provenPeerDeviceId = null),
        )
    }

    /**
     * A proof that does not verify — here, one minted over a *different*
     * session's transcript and replayed onto this one. Core refuses it, so the
     * guard is handed no device id and fails loud.
     */
    @Test
    fun `a proof that does not verify on this session is flagged`() {
        val fleet = Fleet()
        val recorded = transcript(0xB2)
        val live = transcript(0xC3)
        val replayed = proof(fleet.sibling, recorded)
        assertNull(
            "a proof bound to one transcript must not open against another",
            opened(fleet, replayed, live),
        )
        assertEquals(
            OwnIdentityClonePolicy.Verdict.CLONE,
            verdictForPeerHoldingOurKey(fleet, opened(fleet, replayed, live)),
        )
    }

    /**
     * The reflection: a host this phone dialed decrypts the proof it was sent
     * and hands the same plaintext straight back. Both ends of one handshake
     * share a transcript, so only the role tag and this phone's own device id
     * stop it verifying, naming this phone, and being found in this phone's own
     * roster.
     */
    @Test
    fun `our own proof handed back to us is flagged`() {
        val fleet = Fleet()
        val session = transcript(0xD4)
        val ours = proof(fleet.thisPhone, session, CoreLanProofRole.INITIATOR)
        assertNull(
            "a device is not its own sibling",
            opened(fleet, ours, session, peerRole = CoreLanProofRole.RESPONDER),
        )
        assertEquals(
            OwnIdentityClonePolicy.Verdict.CLONE,
            verdictForPeerHoldingOurKey(
                fleet,
                opened(fleet, ours, session, peerRole = CoreLanProofRole.RESPONDER),
            ),
        )
    }

    /**
     * A device signing key this person's roster never named — a stranger, or a
     * restored clone that minted a fresh device key on first run because the
     * backup carried none.
     */
    @Test
    fun `a device this roster never named is flagged`() {
        val fleet = Fleet()
        val session = transcript(0xE5)
        val stranger = generateDeviceKeypair()
        assertNull(opened(fleet, proof(stranger, session), session))
        assertEquals(
            OwnIdentityClonePolicy.Verdict.CLONE,
            verdictForPeerHoldingOurKey(fleet, opened(fleet, proof(stranger, session), session)),
        )
    }

    /**
     * **The sticky-warning half.** A recognised sibling stays recognised: two
     * meetings on two different sessions, each with its own transcript and its
     * own freshly minted proof, both answer [OwnIdentityClonePolicy.Verdict
     * .SIBLING] — so nothing is ever handed to `recordIdentityCloneWarning`, and
     * the banner a person dismissed does not come back on the next meeting.
     */
    @Test
    fun `a recognised sibling stays recognised across meetings`() {
        val fleet = Fleet()
        for (session in listOf(transcript(0x11), transcript(0x22))) {
            assertEquals(
                OwnIdentityClonePolicy.Verdict.SIBLING,
                verdictForPeerHoldingOurKey(
                    fleet,
                    opened(fleet, proof(fleet.sibling, session), session),
                ),
            )
        }
    }

    /**
     * A fleet projection this phone cannot read cannot clear anybody, so a peer
     * holding this identity's key is flagged rather than waved through.
     */
    @Test
    fun `a fleet this phone cannot read is flagged`() {
        val fleet = Fleet()
        val session = transcript(0x33)
        assertEquals(
            OwnIdentityClonePolicy.Verdict.CLONE,
            verdictForPeerHoldingOurKey(
                fleet,
                opened(fleet, proof(fleet.sibling, session), session),
                projection = { null },
            ),
        )
    }

    /**
     * A peer whose static key is not this identity's own is none of the guard's
     * business, proof or no proof. This is every contact, every stranger on the
     * Wi-Fi, and — because §9's ceremony gives a linked device an agreement key
     * of its own — every genuine sibling, which is why no warning has ever fired
     * on one.
     *
     * The projection is never asked for on this path, and that is the half worth
     * pinning: the key test has to come first, or a store this phone cannot read
     * would fail loud about a peer that never held this identity's key — the
     * neighbour's phone, warned about as this person's own backup.
     */
    @Test
    fun `a peer that does not hold this identity's key is not judged at all`() {
        val fleet = Fleet()
        val session = transcript(0xF6)
        var projectionReads = 0
        assertEquals(
            OwnIdentityClonePolicy.Verdict.NOT_OUR_IDENTITY,
            OwnIdentityClonePolicy.verdict(
                ownAgreePk = fleet.person.agreePk,
                // A §9-linked phone keeps an agreement key of its own.
                remoteStaticKey = fleet.sibling.agreePk,
                fleet = {
                    projectionReads += 1
                    null
                },
                provenPeerDeviceId = opened(fleet, proof(fleet.sibling, session), session),
            ),
        )
        assertEquals(
            "the key test must answer before this phone's own devices are read",
            0,
            projectionReads,
        )
    }
}
