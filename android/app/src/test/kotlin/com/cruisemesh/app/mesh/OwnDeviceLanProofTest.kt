package com.cruisemesh.app.mesh

import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.BlockingQueue
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreLanOwnDeviceProof
import uniffi.cruisemesh_core.CoreLanProofRole
import uniffi.cruisemesh_core.DeviceKeypair
import uniffi.cruisemesh_core.LanNoiseSession
import uniffi.cruisemesh_core.Roster
import uniffi.cruisemesh_core.coreLinkGenesisRoster
import uniffi.cruisemesh_core.coreLinkSignNewDeviceRoster
import uniffi.cruisemesh_core.coreMintInboxKey
import uniffi.cruisemesh_core.coreOwnDeviceLanProof
import uniffi.cruisemesh_core.coreOwnDeviceLanProofOpen
import uniffi.cruisemesh_core.coreRevokeDevicesRoster
import uniffi.cruisemesh_core.coreRosterNamesASibling
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
 *
 * The second half of this file drives the exchange itself over two real Noise
 * sessions: who writes first, what a stranger gets for asking, and what happens
 * when the far end simply hands our own proof back to us.
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

        /** The genesis roster: the approver alone, before anything was linked. */
        val approverAlone: Roster

        /** Both devices live: what each side held before the removal. */
        val bothLive: Roster

        /**
         * The approver's roster after removing the sibling: the sibling out of
         * `devices`, buried in `tombstones`, and the inbox key generation
         * climbed by §10.1's rotation.
         */
        val afterRemoval: Roster

        init {
            approverAlone = coreLinkGenesisRoster(
                person.signSk,
                approver.signPk,
                approver.agreePk,
            )
            bothLive = coreLinkSignNewDeviceRoster(
                approverAlone,
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

    /** A proof from one device, for one session, minted for one end of it. */
    private fun proof(
        device: DeviceKeypair,
        session: ByteArray,
        role: CoreLanProofRole,
    ): ByteArray = coreOwnDeviceLanProof(device.signSk, session, role)

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

        // The sibling dialed, so it proves as the initiator and the approver
        // opens that proof expecting exactly that end.
        assertEquals(
            fleet.sibling.deviceId.toList(),
            coreOwnDeviceLanProofOpen(
                fleet.bothLive,
                session,
                proof(fleet.sibling, session, CoreLanProofRole.INITIATOR),
                CoreLanProofRole.INITIATOR,
                fleet.approver.deviceId,
            ).deviceIdOrFail().toList(),
        )
        // And the answer, coming back the other way on the same session.
        assertEquals(
            fleet.approver.deviceId.toList(),
            coreOwnDeviceLanProofOpen(
                fleet.bothLive,
                session,
                proof(fleet.approver, session, CoreLanProofRole.RESPONDER),
                CoreLanProofRole.RESPONDER,
                fleet.sibling.deviceId,
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
            proof(fleet.sibling, session, CoreLanProofRole.INITIATOR),
            CoreLanProofRole.INITIATOR,
            fleet.approver.deviceId,
        )
        assertEquals(fleet.sibling.deviceId.toList(), fromRemoved.deviceIdOrFail().toList())
        assertTrue("the approver must know this is the removed device", fromRemoved!!.revoked)

        // The removed phone, verifying the approver against the stale roster it
        // still believes.
        val fromApprover = coreOwnDeviceLanProofOpen(
            fleet.bothLive,
            session,
            proof(fleet.approver, session, CoreLanProofRole.RESPONDER),
            CoreLanProofRole.RESPONDER,
            fleet.sibling.deviceId,
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
            proof(fleet.sibling, session, CoreLanProofRole.INITIATOR),
            CoreLanProofRole.INITIATOR,
            fleet.approver.deviceId,
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
                proof(stranger, session, CoreLanProofRole.INITIATOR),
                CoreLanProofRole.INITIATOR,
                fleet.approver.deviceId,
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
        val recorded = proof(fleet.sibling, transcript(0xE5), CoreLanProofRole.INITIATOR)
        assertNull(
            coreOwnDeviceLanProofOpen(
                fleet.bothLive,
                transcript(0xE6),
                recorded,
                CoreLanProofRole.INITIATOR,
                fleet.approver.deviceId,
            ),
        )
    }

    @Test
    fun `a truncated or padded proof is refused rather than interpreted`() {
        val fleet = Fleet()
        val session = transcript(0xF7)
        val good = proof(fleet.sibling, session, CoreLanProofRole.INITIATOR)
        for (spoiled in listOf(
            ByteArray(0),
            good.copyOf(good.size - 1),
            good + ByteArray(1),
            good.copyOf().also { it[0] = (it[0].toInt() xor 0xff).toByte() },
            good.copyOf().also { it[it.size - 1] = (it[it.size - 1].toInt() xor 1).toByte() },
        )) {
            assertNull(
                coreOwnDeviceLanProofOpen(
                    fleet.bothLive,
                    session,
                    spoiled,
                    CoreLanProofRole.INITIATOR,
                    fleet.approver.deviceId,
                ),
            )
        }
    }

    /**
     * **The gate on ever putting this phone's signing key on the wire.**
     *
     * The initiator proves before the host it dialed has proved anything, so a
     * phone sweeping a ship's `/24` would otherwise hand a stable identifier to
     * every host that answers on the port. A phone whose roster names only
     * itself has nothing to gain for it — there is nobody it could recognise —
     * so it mints nothing. The moment a second device is linked, and ever after
     * it is buried, there is somebody to name.
     */
    @Test
    fun `only a phone whose roster names another device puts a proof on the wire`() {
        val fleet = Fleet()
        assertTrue(
            "a phone that has never linked has no sibling to prove to",
            !coreRosterNamesASibling(fleet.approverAlone, fleet.approver.deviceId),
        )
        assertTrue(coreRosterNamesASibling(fleet.bothLive, fleet.approver.deviceId))
        assertTrue(coreRosterNamesASibling(fleet.bothLive, fleet.sibling.deviceId))
        assertTrue(
            "a grave is somebody to name -- it is who the notice is for",
            coreRosterNamesASibling(fleet.afterRemoval, fleet.approver.deviceId),
        )
    }

    // -----------------------------------------------------------------------
    // The exchange itself, over two real Noise sessions
    // -----------------------------------------------------------------------

    /**
     * Two `LanNoiseSession`s wired to each other through queues instead of a
     * socket: everything [exchangeOwnDeviceProof] touches, with none of the
     * NSD, threading, or Android plumbing around it.
     */
    private class SessionPair {
        private val initiatorIdentity = generateIdentity()
        private val responderIdentity = generateIdentity()
        val initiator = LanNoiseSession(true, initiatorIdentity.agreeSk)
        val responder = LanNoiseSession(false, responderIdentity.agreeSk)
        val toInitiator: BlockingQueue<ByteArray> = ArrayBlockingQueue(8)
        val toResponder: BlockingQueue<ByteArray> = ArrayBlockingQueue(8)

        init {
            responder.readHandshakeMessage(initiator.writeHandshakeMessage())
            initiator.readHandshakeMessage(responder.writeHandshakeMessage())
            responder.readHandshakeMessage(initiator.writeHandshakeMessage())
            check(initiator.isHandshakeFinished() && responder.isHandshakeFinished())
        }

        /** The transcript hash both ends agree on, which is what a proof signs. */
        fun session(): ByteArray = initiator.handshakeHash()!!

        fun channel(initiatorSide: Boolean) = QueueChannel(
            session = if (initiatorSide) initiator else responder,
            outbox = if (initiatorSide) toResponder else toInitiator,
            inbox = if (initiatorSide) toInitiator else toResponder,
        )
    }

    /**
     * The transport's socket channel, minus the socket. The record bound and
     * the "a partial record is not a proof" behaviour are the shipped ones:
     * `decryptRecord` answers null until a frame is whole.
     */
    private class QueueChannel(
        private val session: LanNoiseSession,
        private val outbox: BlockingQueue<ByteArray>,
        private val inbox: BlockingQueue<ByteArray>,
    ) : OwnDeviceProofChannel {
        override fun send(proof: ByteArray) {
            session.encryptFrame(proof).forEach(outbox::put)
        }

        override fun receive(): ByteArray? {
            repeat(4) {
                val record = inbox.poll(5, TimeUnit.SECONDS) ?: return null
                session.decryptRecord(record)?.let { return it }
            }
            return null
        }
    }

    /**
     * Run the dialing half on its own thread, because the exchange is genuinely
     * two-sided: the initiator blocks on an answer the responder only sends
     * after verifying.
     */
    private fun <T> onAnotherThread(work: () -> T): () -> T {
        var result: Result<T>? = null
        val thread = Thread { result = runCatching(work) }
        thread.start()
        return {
            thread.join(20_000)
            result!!.getOrThrow()
        }
    }

    /** Both ends of a genuine sibling meeting, driven end to end. */
    @Test
    fun `the exchange names each device to the other`() {
        val fleet = Fleet()
        val wire = SessionPair()
        val session = wire.session()
        assertEquals(session.toList(), wire.responder.handshakeHash()!!.toList())

        val dialing = onAnotherThread {
            exchangeOwnDeviceProof(
                initiator = true,
                channel = wire.channel(initiatorSide = true),
                mint = { role -> proof(fleet.sibling, session, role) },
                open = { payload, peerRole ->
                    coreOwnDeviceLanProofOpen(
                        fleet.bothLive,
                        session,
                        payload,
                        peerRole,
                        fleet.sibling.deviceId,
                    )
                },
            )
        }
        val answering = exchangeOwnDeviceProof(
            initiator = false,
            channel = wire.channel(initiatorSide = false),
            mint = { role -> proof(fleet.approver, session, role) },
            open = { payload, peerRole ->
                coreOwnDeviceLanProofOpen(
                    fleet.afterRemoval,
                    session,
                    payload,
                    peerRole,
                    fleet.approver.deviceId,
                )
            },
        )

        assertEquals(fleet.sibling.deviceId.toList(), answering?.deviceId?.toList())
        assertTrue("the approver knows which phone it removed", answering!!.revoked)
        assertEquals(fleet.approver.deviceId.toList(), dialing()?.deviceId?.toList())
    }

    /**
     * **A stranger that dials us is told nothing.** The responder reads first
     * and answers only once the proof verifies, so a phone that dials a
     * CruiseMesh listener with a key nobody's roster names gets a closed socket
     * and not one byte of this device's signing key.
     */
    @Test
    fun `a responder that cannot verify the peer never answers`() {
        val fleet = Fleet()
        val stranger = generateDeviceKeypair()
        val wire = SessionPair()
        val session = wire.session()

        val dialing = onAnotherThread {
            exchangeOwnDeviceProof(
                initiator = true,
                channel = wire.channel(initiatorSide = true),
                mint = { role -> proof(stranger, session, role) },
                open = { _, _ -> null },
            )
        }
        val answering = exchangeOwnDeviceProof(
            initiator = false,
            channel = wire.channel(initiatorSide = false),
            mint = { role -> proof(fleet.approver, session, role) },
            open = { payload, peerRole ->
                coreOwnDeviceLanProofOpen(
                    fleet.bothLive,
                    session,
                    payload,
                    peerRole,
                    fleet.approver.deviceId,
                )
            },
        )

        assertNull("a stranger is refused", answering)
        assertTrue("and nothing was written back to it", wire.toInitiator.isEmpty())
        assertNull(dialing())
    }

    /**
     * **The reflection.** A host that answers a dial holds the session
     * legitimately: it can decrypt the proof this phone sends, re-encrypt that
     * same plaintext under its own sending key, and return it. Both ends of one
     * handshake share a transcript hash, so the signature still verifies — and
     * it names this very phone, which this phone's own roster of course lists.
     *
     * Refused because the proof was minted for the *dialing* end and is opened
     * as the answer, and refused again because it derives to this device.
     */
    @Test
    fun `a host that hands our own proof back is not admitted as a sibling`() {
        val fleet = Fleet()
        val wire = SessionPair()
        val session = wire.session()

        val dialing = onAnotherThread {
            exchangeOwnDeviceProof(
                initiator = true,
                channel = wire.channel(initiatorSide = true),
                mint = { role -> proof(fleet.approver, session, role) },
                open = { payload, peerRole ->
                    coreOwnDeviceLanProofOpen(
                        fleet.bothLive,
                        session,
                        payload,
                        peerRole,
                        fleet.approver.deviceId,
                    )
                },
            )
        }

        // The far end, doing the only thing it can: reading what it was handed
        // and handing it straight back.
        val farEnd = wire.channel(initiatorSide = false)
        val received = farEnd.receive()
        assertTrue("the far end really did get our proof", received != null)
        farEnd.send(received!!)

        assertNull("and it buys nothing", dialing())
    }

    private fun CoreLanOwnDeviceProof?.deviceIdOrFail(): ByteArray =
        this?.deviceId ?: throw AssertionError("the roster should have named this device")
}
