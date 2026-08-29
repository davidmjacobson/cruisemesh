import Foundation

/// Whether a LAN link that reached the own-device arm of the handshake is a
/// clone of this person's identity, one of their own devices, or neither
/// (`specs/multi-device-v1.md` §1, §6, §10 step 5).
///
/// A plain enum with no UIKit, no store and no transport in it, so the rule can
/// be read by a unit test rather than only by a running mesh.
///
/// # The two inputs, and why the second one is not optional
///
/// The **key** says whether this peer holds this person's identity outright: its
/// Noise static is this identity's own agreement key. That is a `.cmbak` restore
/// running alongside its source — §1's two-devices-one-author-stream failure —
/// and it is what the warning has always been for.
///
/// The key alone cannot say *which* device is holding it. §6 makes the inbox key
/// person-scoped and generation 0 of it is the deployed person agreement key —
/// `core/src/device_link/bootstrap.rs` seeds it straight from `Identity.agree_pk`
/// and `agree_sk` — so "holds this person's agreement key" is a fact about a
/// *person*, not about a device.
///
/// The **proof** is what names the device: §10 step 5's roster proof, a
/// signature over this session's Noise transcript hash under a device signing
/// key, opened against the roster this phone holds
/// (`coreOwnDeviceLanProofOpen`). `provenPeerDeviceId` is that function's answer
/// for this link and nothing weaker — never a device id read out of a frame,
/// never a user id from a HELLO.
///
/// # What the proof does and does not settle
///
/// It settles that the peer holds the secret half of a device signing key this
/// person's roster names, **on this session**. The transcript hash is unique to
/// one handshake and the signed bytes carry a role tag naming which end minted
/// them, so a proof recorded off one session is worthless on the next, and a
/// host cannot re-encrypt the one we sent it and hand it back as its own.
///
/// It does not settle that the peer is distinct hardware. What makes it a real
/// discriminator against the case this guard exists for is where the key lives:
/// a `.cmbak` carries the person identity (`core/src/backup.rs`) and the sqlite
/// store, and no device signing secret at all — those are minted per install by
/// `DeviceKeyStore` and kept in the keychain. So a restored clone mints a fresh
/// device key the roster has never heard of, its proof opens against nothing,
/// and it is flagged. A peer that extracted a device signing secret off a phone
/// would not be, and no LAN handshake can tell.
///
/// # What is reachable today, stated plainly
///
/// On today's wire this rule answers `.sibling` for nobody, and that is a
/// property of the handshake rather than a gap in the rule. §10 step 5 keeps the
/// clone arm *symmetric*: two ends that each see their own agreement key coming
/// back take that arm together and exchange no proof frame, because on a link
/// where one would never arrive, waiting for one is a hang. So the arm that
/// fires this guard hands it a nil device id every time, and the answer is
/// `.clone` exactly as it was before this rule existed. Every genuine sibling
/// §9's ceremony links keeps an agreement key of its own and never trips the key
/// test at all, which is why no warning has fired on one.
///
/// What changed is that the answer is now *derived* from the same two facts §10
/// step 5's admission bar uses (`OwnRosterNoticePolicy`), by one rule both shells
/// share and a test can read. Before this, Android asked core with a hardcoded
/// null and this shell never asked core at all — two shells, two different
/// unreachable-by-construction answers to one question.
///
/// Making the clone arm itself proof-bearing is a wire change, not a call-site
/// one: against a peer running an older build the new end would block on a proof
/// that is never sent, and the real clone would go unwarned. That belongs behind
/// a capability bit and §12's rollout discipline.
///
/// # Failing loud
///
/// No proof means `.clone`, not "probably fine". A peer holding this person's
/// identity that cannot say which of their devices it is *is* the situation the
/// warning exists for, and a person told about a sibling once is better served
/// than a person never told about a clone. A fleet projection this phone cannot
/// read is the same answer for the same reason.
///
/// The key test runs *before* that, and the order is load-bearing: failing loud
/// on a peer whose static key was never this identity's own would warn a person
/// about the neighbour's phone.
///
/// One consequence to keep in view for the day the clone arm can carry a proof:
/// a revoked device is absent from `OwnDeviceFleet.deviceIds` — tombstones stay
/// in the roster document, not in the projection — so a tombstoned sibling would
/// answer `.clone` here even though §10 step 5 deliberately *admits* its link so
/// the removal notice can reach it. Admission and this warning are different
/// questions, and that one is not settled here.
///
/// Mirrors Android's `OwnIdentityClonePolicy.kt`.
enum OwnIdentityClonePolicy {

    /// What one own-device link's pair of facts adds up to.
    enum Verdict: Equatable {
        /// Not this person's identity at all. Nothing to record, nothing to say.
        case notOurIdentity
        /// A device this person's own roster names. Expected, never warned about.
        case sibling
        /// This person's identity on a device their roster cannot name. Warn.
        case clone
    }

    /// - Parameters:
    ///   - ownAgreePk: this identity's own agreement key.
    ///   - remoteStaticKey: the Noise static key the far end proved it holds
    ///     during the handshake.
    ///   - fleet: this person's own devices as core projects them, or nil when
    ///     this phone could not read the projection. A closure rather than a
    ///     value so the ordering the "failing loud" note above describes lives
    ///     inside the rule a test can read: a peer that does not hold this
    ///     identity's key is answered without the projection being asked for at
    ///     all, which is what stops an unreadable store from convicting a
    ///     stranger.
    ///   - provenPeerDeviceId: what `coreOwnDeviceLanProofOpen` returned for
    ///     *this* session, or nil when the far end proved no such thing —
    ///     because it sent no proof, because the proof did not verify against
    ///     this session's transcript, or because the device it named is not in
    ///     this person's roster.
    static func verdict(
        ownAgreePk: Data,
        remoteStaticKey: Data,
        fleet: () -> OwnDeviceFleet?,
        provenPeerDeviceId: Data?
    ) -> Verdict {
        guard ownLanStaticKeyMatches(ownAgreePk: ownAgreePk, remoteStaticKey: remoteStaticKey) else {
            return .notOurIdentity
        }
        guard let projection = fleet() else { return .clone }
        switch coreOwnIdentityPeer(fleet: projection, peerDeviceId: provenPeerDeviceId) {
        case .sibling: return .sibling
        case .clone: return .clone
        }
    }
}
