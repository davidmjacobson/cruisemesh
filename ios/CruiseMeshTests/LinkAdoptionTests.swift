import XCTest
@testable import CruiseMesh

/// The person's own name and photo crossing §9's export, both ends.
///
/// The bug this pins is not subtle once stated: the link export carried no
/// profile at all, so a phone that had just been adopted asked the person their
/// own name — and any answer was a second name for one person, which is a fleet
/// profile fork nothing in v1 reconciles.
///
/// The Swift twin of Android's `LinkAdoptionTest`.
final class LinkAdoptionTests: XCTestCase {
    private let displayNameKey = "cruisemesh.displayName"
    private let avatarEpochKey = "cruisemesh.ownAvatarEpoch"
    private let onboardingKey = "cruisemesh.onboarding.completed"
    private let permissionsStepKey = "cruisemesh.onboarding.permissionsStepDone"
    private let photo = Data((0..<64).map { UInt8($0) })
    private let epoch: Int64 = 1_755_000_000_000

    override func setUp() {
        super.setUp()
        clearStores()
    }

    override func tearDown() {
        clearStores()
        super.tearDown()
    }

    private func clearStores() {
        AppDefaults.current.removeObject(forKey: displayNameKey)
        AppDefaults.current.removeObject(forKey: avatarEpochKey)
        AppDefaults.current.removeObject(forKey: onboardingKey)
        AppDefaults.current.removeObject(forKey: permissionsStepKey)
        ProfilePhotoStore.clear()
    }

    func testTheApprovingDeviceSendsTheNameItsContactsAlreadySee() {
        XCTAssertTrue(ProfileStore.saveDisplayName("Maya"))
        ProfilePhotoStore.restoreBackupBytes(photo)
        ProfileStore.restoreOwnAvatarEpoch(epoch)

        let profile = LinkAdoption.profileOf()

        XCTAssertEqual(profile.displayName, "Maya")
        XCTAssertEqual(profile.avatar, photo)
        XCTAssertEqual(profile.avatarEpoch, epoch)
    }

    /// Not empty and not nil: this is the name this person's contacts see today,
    /// so a phone joining them must not disagree about it.
    func testAPersonWhoNeverChoseANameStillSendsTheOneTheyAreShownUnder() {
        XCTAssertEqual(LinkAdoption.profileOf().displayName, ProfileStore.defaultDisplayName)
    }

    func testAnAdoptedPhoneTakesTheProfileAndStopsBeingUnsetUp() {
        LinkAdoption.adopted(
            profile: LinkBootstrapProfile(displayName: "Maya", avatar: photo, avatarEpoch: epoch)
        )

        XCTAssertEqual(ProfileStore.loadStoredDisplayName(), "Maya")
        XCTAssertEqual(ProfilePhotoStore.loadBackupBytes(), photo)
        // Restored, never bumped: this is the number profile sync orders updates
        // by, so a fresh one here would make the newest phone outrank the fleet
        // it just joined and re-broadcast the person's profile.
        XCTAssertEqual(ProfileStore.loadOwnAvatarEpoch(), epoch)
        // And the field the person was being asked to fill in is filled in, so
        // first-run setup has nothing left to ask.
        XCTAssertTrue(OnboardingStore.isCompleted())
        XCTAssertFalse(ProfileStore.loadStoredDisplayName().isEmpty)
        // ...except the one thing this route went around. The wizard carries
        // the permissions step; an adopted phone never saw it, and used to land
        // on the chat list with the mesh off and nothing on screen saying why.
        // `FirstRunRouter` collects this on the way in.
        XCTAssertEqual(OnboardingStore.permissionsStepDone(), false)
    }

    func testAnExportWithNoNameLeavesTheQuestionOpenRatherThanBlankingIt() {
        XCTAssertTrue(ProfileStore.saveDisplayName("Maya"))

        LinkAdoption.adopted(
            profile: LinkBootstrapProfile(displayName: nil, avatar: Data(), avatarEpoch: 0)
        )

        XCTAssertEqual(ProfileStore.loadStoredDisplayName(), "Maya")
        XCTAssertTrue(OnboardingStore.isCompleted())
    }
}
