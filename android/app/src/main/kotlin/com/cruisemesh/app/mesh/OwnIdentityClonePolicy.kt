package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreOwnIdentityPeer
import uniffi.cruisemesh_core.OwnDeviceFleet
import uniffi.cruisemesh_core.coreOwnIdentityPeer

/**
 * Whether a LAN link that reached the own-device arm of the handshake is a
 * clone of this person's identity, one of their own devices, or neither
 * (`specs/multi-device-v1.md` §1, §6, §10 step 5).
 *
 * A plain object with no Android imports, so the rule can be read by a unit
 * test instead of only by a foreground service.
 *
 * # The two inputs, and why the second one is not optional
 *
 * The **key** says whether this peer holds this person's identity outright: its
 * Noise static is this identity's own agreement key. That is a `.cmbak` restore
 * running alongside its source — §1's two-devices-one-author-stream failure —
 * and it is what the warning has always been for.
 *
 * The key alone cannot say *which* device is holding it. §6 makes the inbox key
 * person-scoped and generation 0 of it is the deployed person agreement key —
 * `core/src/device_link/bootstrap.rs` seeds it straight from `Identity.agree_pk`
 * and `agree_sk` — so "holds this person's agreement key" is a fact about a
 * *person*, not about a device.
 *
 * The **proof** is what names the device: §10 step 5's roster proof, a signature
 * over this session's Noise transcript hash under a device signing key, opened
 * against the roster this phone holds
 * (`uniffi.cruisemesh_core.coreOwnDeviceLanProofOpen`). `provenPeerDeviceId` is
 * that function's answer for this link and nothing weaker — never a device id
 * read out of a frame, never a user id from a HELLO.
 *
 * # What the proof does and does not settle
 *
 * It settles that the peer holds the secret half of a device signing key this
 * person's roster names, **on this session**. The transcript hash is unique to
 * one handshake and the signed bytes carry a role tag naming which end minted
 * them, so a proof recorded off one session is worthless on the next, and a host
 * cannot re-encrypt the one we sent it and hand it back as its own.
 *
 * It does not settle that the peer is distinct hardware. What makes it a real
 * discriminator against the case this guard exists for is where the key lives: a
 * `.cmbak` carries the person identity (`core/src/backup.rs`) and the sqlite
 * store, and no device signing secret at all — those are minted per install by
 * [com.cruisemesh.app.identity.DeviceKeyStore] and wrapped under a key that
 * never leaves the Android Keystore. So a restored clone mints a fresh device
 * key the roster has never heard of, its proof opens against nothing, and it is
 * flagged. A peer that extracted a device signing secret off a phone would not
 * be, and no LAN handshake can tell.
 *
 * # What is reachable today, stated plainly
 *
 * On today's wire this rule answers [Verdict.SIBLING] for nobody, and that is a
 * property of the handshake rather than a gap in the rule. §10 step 5 keeps the
 * clone arm *symmetric*: two ends that each see their own agreement key coming
 * back take that arm together and exchange no proof frame, because on a link
 * where one would never arrive, waiting for one is a hang. So the arm that fires
 * this guard hands it a null device id every time, and the answer is
 * [Verdict.CLONE] exactly as it was before this rule existed. Every genuine
 * sibling §9's ceremony links keeps an agreement key of its own and never trips
 * the key test at all, which is why no warning has fired on one.
 *
 * What changed is that the answer is now *derived* from the same two facts §10
 * step 5's admission bar uses ([OwnRosterNoticePolicy]), by one rule both shells
 * share and a test can read. Before this, Android asked core with a hardcoded
 * null and iOS never asked core at all — two shells, two different
 * unreachable-by-construction answers to one question.
 *
 * Making the clone arm itself proof-bearing is a wire change, not a call-site
 * one: against a peer running an older build the new end would block on a proof
 * that is never sent, and the real clone would go unwarned. That belongs behind a
 * capability bit and §12's rollout discipline.
 *
 * # Failing loud
 *
 * No proof means [Verdict.CLONE], not "probably fine". A peer holding this
 * person's identity that cannot say which of their devices it is *is* the
 * situation the warning exists for, and a person told about a sibling once is
 * better served than a person never told about a clone. A fleet projection this
 * phone cannot read is the same answer for the same reason.
 *
 * The key test runs *before* that, and the order is load-bearing: failing loud
 * on a peer whose static key was never this identity's own would warn a person
 * about the neighbour's phone.
 *
 * One consequence to keep in view for the day the clone arm can carry a proof: a
 * revoked device is absent from [OwnDeviceFleet.deviceIds] — tombstones stay in
 * the roster document, not in the projection — so a tombstoned sibling would
 * answer [Verdict.CLONE] here even though §10 step 5 deliberately *admits* its
 * link so the removal notice can reach it. Admission and this warning are
 * different questions, and that one is not settled here.
 */
internal object OwnIdentityClonePolicy {

    /** What one own-device link's pair of facts adds up to. */
    enum class Verdict {
        /** Not this person's identity at all. Nothing to record, nothing to say. */
        NOT_OUR_IDENTITY,

        /** A device this person's own roster names. Expected, and never warned about. */
        SIBLING,

        /** This person's identity on a device their roster cannot name. Warn. */
        CLONE,
    }

    /**
     * @param ownAgreePk this identity's own agreement key.
     * @param remoteStaticKey the Noise static key the far end proved it holds
     *   during the handshake.
     * @param fleet this person's own devices as core projects them, or null when
     *   this phone could not read the projection. A supplier rather than a value
     *   so the ordering the "failing loud" note above describes lives inside the
     *   rule a test can read: a peer that does not hold this identity's key is
     *   answered without the projection being asked for at all, which is what
     *   stops an unreadable store from convicting a stranger.
     * @param provenPeerDeviceId what `coreOwnDeviceLanProofOpen` returned for
     *   *this* session, or null when the far end proved no such thing — because
     *   it sent no proof, because the proof did not verify against this
     *   session's transcript, or because the device it named is not in this
     *   person's roster.
     */
    fun verdict(
        ownAgreePk: ByteArray,
        remoteStaticKey: ByteArray,
        fleet: () -> OwnDeviceFleet?,
        provenPeerDeviceId: ByteArray?,
    ): Verdict {
        if (!ownLanStaticKeyMatches(ownAgreePk, remoteStaticKey)) return Verdict.NOT_OUR_IDENTITY
        val projection = fleet() ?: return Verdict.CLONE
        return when (coreOwnIdentityPeer(projection, provenPeerDeviceId)) {
            CoreOwnIdentityPeer.SIBLING -> Verdict.SIBLING
            CoreOwnIdentityPeer.CLONE -> Verdict.CLONE
        }
    }
}
