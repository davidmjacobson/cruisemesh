import XCTest
@testable import CruiseMesh

final class ProfileStoreTests: XCTestCase {
    private let displayNameKey = "cruisemesh.displayName"

    override func setUp() {
        super.setUp()
        AppDefaults.current.removeObject(forKey: displayNameKey)
    }

    override func tearDown() {
        AppDefaults.current.removeObject(forKey: displayNameKey)
        super.tearDown()
    }

    func testBlankEditKeepsTheLastRealName() {
        XCTAssertTrue(ProfileStore.saveDisplayName("  Maya  "))
        XCTAssertFalse(ProfileStore.saveDisplayName("   "))

        XCTAssertEqual(ProfileStore.loadStoredDisplayName(), "Maya")
        XCTAssertEqual(ProfileStore.loadDisplayName(), "Maya")
    }

    func testMissingLegacyNameUsesFallbackOutsideOnboarding() {
        XCTAssertEqual(ProfileStore.loadStoredDisplayName(), "")
        XCTAssertEqual(ProfileStore.loadDisplayName(), "CruiseMesh user")
    }
}
