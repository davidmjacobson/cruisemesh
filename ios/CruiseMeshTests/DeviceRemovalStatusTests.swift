import XCTest
@testable import CruiseMesh

/// §10 step 5 as the removed phone lives it, from this shell's side.
///
/// The field session on 2026-08-18 is the scenario, in full: the approving phone
/// removes a device; four minutes later, on the same Wi-Fi, the removed phone
/// still holds the old device list, still says the mesh is on, and still behaves
/// as though it is linked. With no contacts and no Shore Pass there was no
/// indirect signal either, so waiting longer would never have fixed it.
///
/// What is pinned here is the shell half: the process-wide answer the screens
/// read must flip on the notice, and it must not flip for a document that does
/// not deserve it. `LinkVisibility` is deliberately left out — on this shell it
/// drives the live `MeshController` singleton, which is not a thing a unit test
/// should be reaching into; Android's twin covers that half.
///
/// The Swift twin of Android's `DeviceRemovalStatusTest`.
final class DeviceRemovalStatusTests: XCTestCase {
    private let nowMs: Int64 = 1_755_000_000_000

    override func tearDown() {
        // A process-wide singleton, so a test that left it set would carry into
        // the next. Put it back the way a fresh install reads it.
        if let fresh = try? MessageStore.open(path: ":memory:") {
            DeviceRemovalStatus.shared.refresh(store: fresh)
        }
        super.tearDown()
    }

    func testAnInstallThatWasNeverRemovedSaysSo() throws {
        let fleet = try Fleet.link()

        DeviceRemovalStatus.shared.refresh(store: fleet.removedStore)

        XCTAssertFalse(DeviceRemovalStatus.shared.isRemoved)
    }

    func testASignedListThatBuriesThisPhoneStandsItDown() throws {
        let fleet = try Fleet.link()

        let adoption = try fleet.removedStore.applyOwnRosterNotice(
            document: try coreEncodeRoster(roster: fleet.rosterWithoutTheSecondDevice()),
            personRootSignPk: fleet.identity.signPk,
            ownDeviceId: fleet.second.deviceId,
            nowMs: nowMs
        )

        XCTAssertEqual(adoption.outcome, .revokedSelf)
        // Core's own answer: terminal, and the gate refuses everything from it.
        XCTAssertEqual(try fleet.removedStore.linkActivation().stage, .revoked)
        // The shell's: the screens change.
        DeviceRemovalStatus.shared.refresh(store: fleet.removedStore)
        XCTAssertTrue(DeviceRemovalStatus.shared.isRemoved)
    }

    /// And it stays told. A device ejected in one process must still know on the
    /// next launch, which is why the answer is read from the stage rather than
    /// remembered as an event.
    func testTheAnswerSurvivesTheProcessThatHeardIt() throws {
        let fleet = try Fleet.link()
        _ = try fleet.removedStore.applyOwnRosterNotice(
            document: try coreEncodeRoster(roster: fleet.rosterWithoutTheSecondDevice()),
            personRootSignPk: fleet.identity.signPk,
            ownDeviceId: fleet.second.deviceId,
            nowMs: nowMs
        )

        DeviceRemovalStatus.shared.refresh(store: try MessageStore.open(path: ":memory:"))
        XCTAssertFalse(DeviceRemovalStatus.shared.isRemoved)

        DeviceRemovalStatus.shared.refresh(store: fleet.removedStore)
        XCTAssertTrue(DeviceRemovalStatus.shared.isRemoved)
    }

    /// The other phone in the fleet reads the same document and is untouched by
    /// it: a notice is not a broadcast stop button, it is one person's own list
    /// saying who is on it.
    ///
    /// It does not adopt it either, and that is core's rule rather than an
    /// omission: a removal rotates the fleet's inbox key, a plaintext link frame
    /// carries no key material, and a sibling that took the list here would hold
    /// a fleet whose own traffic it cannot open. §10.1's sealed handoff is what
    /// closes that, so the honest answer is "still waiting for the key".
    func testASiblingThatIsStillListedWaitsForItsKeyInstead() throws {
        let fleet = try Fleet.link()

        let adoption = try fleet.approvingStore.applyOwnRosterNotice(
            document: try coreEncodeRoster(roster: fleet.rosterWithoutTheSecondDevice()),
            personRootSignPk: fleet.identity.signPk,
            ownDeviceId: fleet.first.deviceId,
            nowMs: nowMs
        )

        XCTAssertEqual(adoption.outcome, .awaitingRotationKey)
        DeviceRemovalStatus.shared.refresh(store: fleet.approvingStore)
        XCTAssertFalse(DeviceRemovalStatus.shared.isRemoved)
    }

    /// A device list nobody's person signed changes nothing. Core is what
    /// enforces this — the shell's link test is the layer above it — and this
    /// says so out loud, because "somebody can post a document that bricks your
    /// phone" is exactly the failure this whole mechanism must not introduce.
    func testAListSignedBySomebodyElseChangesNothing() throws {
        let fleet = try Fleet.link()
        let stranger = generateIdentity()
        let strangerDevice = generateDeviceKeypair()
        let forged = try coreLinkGenesisRoster(
            personRootSignSk: stranger.signSk,
            deviceSignPk: strangerDevice.signPk,
            deviceAgreePk: strangerDevice.agreePk
        )

        // Refused either as an error or as an outcome; both are a refusal, and
        // neither may touch the stage.
        if let adoption = try? fleet.removedStore.applyOwnRosterNotice(
            document: try coreEncodeRoster(roster: forged),
            personRootSignPk: fleet.identity.signPk,
            ownDeviceId: fleet.second.deviceId,
            nowMs: nowMs
        ) {
            XCTAssertEqual(adoption.outcome, .refused)
        }

        XCTAssertEqual(try fleet.removedStore.linkActivation().stage, .notLinking)
        DeviceRemovalStatus.shared.refresh(store: fleet.removedStore)
        XCTAssertFalse(DeviceRemovalStatus.shared.isRemoved)
    }

    /// One person, two phones, one store each — §9's ceremony reduced to the
    /// documents it produces, which is all this file needs.
    private struct Fleet {
        let identity: Identity
        let first: DeviceKeypair
        let second: DeviceKeypair
        let roster: Roster
        let approvingStore: MessageStore
        let removedStore: MessageStore

        /// §10.1's update, signed by the phone that holds the signing role.
        func rosterWithoutTheSecondDevice() throws -> Roster {
            try coreRevokeDevicesRoster(
                current: roster,
                personRootSignPk: identity.signPk,
                approvingDeviceSignSk: first.signSk,
                revokedDeviceIds: [second.deviceId],
                // Generation 0 is the deployed person agreement key (§10 note 4),
                // so it is derived rather than stored -- see InboxKeyStore.
                currentInboxKey: InboxKey(
                    generation: 0,
                    agreePk: identity.agreePk,
                    agreeSk: identity.agreeSk
                )
            ).roster
        }

        static func link() throws -> Fleet {
            let identity = generateIdentity()
            let first = generateDeviceKeypair()
            let second = generateDeviceKeypair()
            let genesis = try coreLinkGenesisRoster(
                personRootSignSk: identity.signSk,
                deviceSignPk: first.signPk,
                deviceAgreePk: first.agreePk
            )
            let update = try coreLinkSignNewDeviceRoster(
                current: genesis,
                personRootSignPk: identity.signPk,
                approvingDeviceSignSk: first.signSk,
                newDeviceSignPk: second.signPk,
                newDeviceAgreePk: second.agreePk
            )
            let approvingStore = try MessageStore.open(path: ":memory:")
            try approvingStore.adoptOwnRoster(
                roster: update.roster,
                personRootSignPk: identity.signPk,
                ownDeviceId: first.deviceId
            )
            let removedStore = try MessageStore.open(path: ":memory:")
            try removedStore.adoptOwnRoster(
                roster: update.roster,
                personRootSignPk: identity.signPk,
                ownDeviceId: second.deviceId
            )
            return Fleet(
                identity: identity,
                first: first,
                second: second,
                roster: update.roster,
                approvingStore: approvingStore,
                removedStore: removedStore
            )
        }
    }
}
