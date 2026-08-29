import XCTest
@testable import CruiseMesh

/// The clone guard's decision, driven with the same inputs the LAN transport
/// hands it (`specs/multi-device-v1.md` §1, §6, §10 step 5).
///
/// The `provenPeerDeviceId` argument is never invented here. Every case mints a
/// real §10 step 5 proof and opens it through `coreOwnDeviceLanProofOpen`, so
/// "no proof", "a proof that does not verify" and "a proof from a device this
/// roster never named" are three distinct journeys to the same nil rather than
/// three spellings of one literal — which is the difference between testing the
/// guard and testing the test.
///
/// What the guard rests on, and what it does not: a proof says the far end holds
/// the secret half of a device signing key this person's roster names, on this
/// session. It rules out replay (the transcript is unique to one handshake and
/// the signed bytes name which end minted them) and it rules out a `.cmbak`
/// restore, which carries the person identity but no device signing secret. It
/// says nothing about distinct hardware, and a device whose signing secret was
/// extracted would still clear it.
///
/// One thing these cases do NOT claim: that a sibling is recognised on today's
/// LAN. §10 step 5 keeps the clone arm of the handshake symmetric and
/// proof-free, so the arm that fires this guard always hands it nil and the
/// answer is always `.clone`. What is pinned here is the rule, and that both
/// shells now compute it from the same two facts instead of one shell hardcoding
/// a null and the other never asking.
///
/// The Swift twin of Android's `OwnIdentityClonePolicyTest`.
final class OwnIdentityClonePolicyTests: XCTestCase {

    /// Two devices of one person, built the way §9's ceremony builds them.
    private struct Fleet {
        let person: Identity
        let thisPhone: DeviceKeypair
        let sibling: DeviceKeypair
        let roster: Roster

        init() throws {
            let person = generateIdentity()
            let thisPhone = generateDeviceKeypair()
            let sibling = generateDeviceKeypair()
            let genesis = try coreLinkGenesisRoster(
                personRootSignSk: person.signSk,
                deviceSignPk: thisPhone.signPk,
                deviceAgreePk: thisPhone.agreePk
            )
            self.roster = try coreLinkSignNewDeviceRoster(
                current: genesis,
                personRootSignPk: person.signPk,
                approvingDeviceSignSk: thisPhone.signSk,
                newDeviceSignPk: sibling.signPk,
                newDeviceAgreePk: sibling.agreePk
            ).roster
            self.person = person
            self.thisPhone = thisPhone
            self.sibling = sibling
        }

        /// What core projects for this phone out of that roster.
        var fleetRecord: OwnDeviceFleet {
            OwnDeviceFleet(
                ownDeviceId: thisPhone.deviceId,
                deviceIds: [thisPhone.deviceId, sibling.deviceId],
                projectedFrom: RosterVersion(recoveryEpoch: 0, seq: roster.seq)
            )
        }
    }

    /// Stands in for a Noise transcript hash; two values are two sessions.
    private func transcript(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }

    private func proof(
        _ device: DeviceKeypair,
        session: Data,
        role: CoreLanProofRole = .initiator
    ) throws -> Data {
        try coreOwnDeviceLanProof(deviceSignSk: device.signSk, handshakeHash: session, role: role)
    }

    /// What this phone would learn from a peer's proof on one session — exactly
    /// the value `LanTransport` passes the guard.
    private func opened(
        _ fleet: Fleet,
        _ payload: Data,
        session: Data,
        peerRole: CoreLanProofRole = .initiator
    ) -> Data? {
        coreOwnDeviceLanProofOpen(
            roster: fleet.roster,
            handshakeHash: session,
            payload: payload,
            peerRole: peerRole,
            ownDeviceId: fleet.thisPhone.deviceId
        )?.deviceId
    }

    /// The verdict for a peer whose Noise static key *is* this identity's own
    /// agreement key — the only peers the guard has an opinion about.
    private func verdictForPeerHoldingOurKey(
        _ fleet: Fleet,
        provenPeerDeviceId: Data?
    ) -> OwnIdentityClonePolicy.Verdict {
        verdictForPeerHoldingOurKey(
            fleet,
            provenPeerDeviceId: provenPeerDeviceId,
            projection: { fleet.fleetRecord }
        )
    }

    private func verdictForPeerHoldingOurKey(
        _ fleet: Fleet,
        provenPeerDeviceId: Data?,
        projection: () -> OwnDeviceFleet?
    ) -> OwnIdentityClonePolicy.Verdict {
        OwnIdentityClonePolicy.verdict(
            ownAgreePk: fleet.person.agreePk,
            remoteStaticKey: Data(fleet.person.agreePk),
            fleet: projection,
            provenPeerDeviceId: provenPeerDeviceId
        )
    }

    /// **The case the whole change exists for.** A device this person's own
    /// roster names, which proved it on this session, is not a clone — however
    /// the key test reads.
    func testASiblingThatProvedItselfIsNotFlagged() throws {
        let fleet = try Fleet()
        let session = transcript(0xA1)
        let proven = opened(fleet, try proof(fleet.sibling, session: session), session: session)
        XCTAssertEqual(proven, fleet.sibling.deviceId)
        XCTAssertEqual(verdictForPeerHoldingOurKey(fleet, provenPeerDeviceId: proven), .sibling)
    }

    /// A peer holding this person's identity that produced no proof at all. The
    /// `.cmbak` restore of §1 is exactly this: it carries the person identity
    /// and the message store, and no device signing secret, so it has nothing
    /// the roster could name.
    func testAPeerThatProvedNothingIsFlagged() throws {
        let fleet = try Fleet()
        XCTAssertEqual(verdictForPeerHoldingOurKey(fleet, provenPeerDeviceId: nil), .clone)
    }

    /// A proof that does not verify — here, one minted over a *different*
    /// session's transcript and replayed onto this one. Core refuses it, so the
    /// guard is handed no device id and fails loud.
    func testAProofThatDoesNotVerifyOnThisSessionIsFlagged() throws {
        let fleet = try Fleet()
        let recorded = transcript(0xB2)
        let live = transcript(0xC3)
        let replayed = try proof(fleet.sibling, session: recorded)
        XCTAssertNil(opened(fleet, replayed, session: live))
        XCTAssertEqual(
            verdictForPeerHoldingOurKey(
                fleet,
                provenPeerDeviceId: opened(fleet, replayed, session: live)
            ),
            .clone
        )
    }

    /// The reflection: a host this phone dialed decrypts the proof it was sent
    /// and hands the same plaintext straight back. Both ends of one handshake
    /// share a transcript, so only the role tag and this phone's own device id
    /// stop it verifying, naming this phone, and being found in this phone's own
    /// roster.
    func testOurOwnProofHandedBackToUsIsFlagged() throws {
        let fleet = try Fleet()
        let session = transcript(0xD4)
        let ours = try proof(fleet.thisPhone, session: session, role: .initiator)
        let reflected = opened(fleet, ours, session: session, peerRole: .responder)
        XCTAssertNil(reflected)
        XCTAssertEqual(verdictForPeerHoldingOurKey(fleet, provenPeerDeviceId: reflected), .clone)
    }

    /// A device signing key this person's roster never named — a stranger, or a
    /// restored clone that minted a fresh device key on first run because the
    /// backup carried none.
    func testADeviceThisRosterNeverNamedIsFlagged() throws {
        let fleet = try Fleet()
        let session = transcript(0xE5)
        let stranger = generateDeviceKeypair()
        let named = opened(fleet, try proof(stranger, session: session), session: session)
        XCTAssertNil(named)
        XCTAssertEqual(verdictForPeerHoldingOurKey(fleet, provenPeerDeviceId: named), .clone)
    }

    /// **The sticky-warning half.** A recognised sibling stays recognised: two
    /// meetings on two different sessions, each with its own transcript and its
    /// own freshly minted proof, both answer `.sibling` — so nothing is ever
    /// handed to `recordIdentityCloneWarning`, and the banner a person dismissed
    /// does not come back on the next meeting.
    func testARecognisedSiblingStaysRecognisedAcrossMeetings() throws {
        let fleet = try Fleet()
        for session in [transcript(0x11), transcript(0x22)] {
            let proven = opened(fleet, try proof(fleet.sibling, session: session), session: session)
            XCTAssertEqual(verdictForPeerHoldingOurKey(fleet, provenPeerDeviceId: proven), .sibling)
        }
    }

    /// A fleet projection this phone cannot read cannot clear anybody, so a peer
    /// holding this identity's key is flagged rather than waved through.
    func testAFleetThisPhoneCannotReadIsFlagged() throws {
        let fleet = try Fleet()
        let session = transcript(0x33)
        let proven = opened(fleet, try proof(fleet.sibling, session: session), session: session)
        XCTAssertEqual(
            verdictForPeerHoldingOurKey(
                fleet,
                provenPeerDeviceId: proven,
                projection: { nil }
            ),
            .clone
        )
    }

    /// A peer whose static key is not this identity's own is none of the guard's
    /// business, proof or no proof. This is every contact, every stranger on the
    /// Wi-Fi, and — because §9's ceremony gives a linked device an agreement key
    /// of its own — every genuine sibling, which is why no warning has ever fired
    /// on one.
    ///
    /// The projection is never asked for on this path, and that is the half worth
    /// pinning: the key test has to come first, or a store this phone cannot read
    /// would fail loud about a peer that never held this identity's key — the
    /// neighbour's phone, warned about as this person's own backup.
    func testAPeerThatDoesNotHoldThisIdentitysKeyIsNotJudgedAtAll() throws {
        let fleet = try Fleet()
        let session = transcript(0xF6)
        let proven = opened(fleet, try proof(fleet.sibling, session: session), session: session)
        var projectionReads = 0
        XCTAssertEqual(
            OwnIdentityClonePolicy.verdict(
                ownAgreePk: fleet.person.agreePk,
                // A §9-linked phone keeps an agreement key of its own.
                remoteStaticKey: fleet.sibling.agreePk,
                fleet: {
                    projectionReads += 1
                    return nil
                },
                provenPeerDeviceId: proven
            ),
            .notOurIdentity
        )
        XCTAssertEqual(
            projectionReads,
            0,
            "the key test must answer before this phone's own devices are read"
        )
    }
}
