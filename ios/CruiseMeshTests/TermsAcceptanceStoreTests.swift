import XCTest
@testable import CruiseMesh

final class TermsAcceptanceStoreTests: XCTestCase {
    func testOnlyCurrentTermsVersionIsAccepted() {
        XCTAssertEqual(TermsAcceptanceStore.currentVersion, "2026-08-08")
        XCTAssertTrue(TermsAcceptanceStore.isCurrentTermsVersion(TermsAcceptanceStore.currentVersion))
        XCTAssertFalse(TermsAcceptanceStore.isCurrentTermsVersion(nil))
        XCTAssertFalse(TermsAcceptanceStore.isCurrentTermsVersion("2026-07-23"))
        XCTAssertFalse(TermsAcceptanceStore.isCurrentTermsVersion("accepted"))
    }

    func testAcceptanceIsVersioned() throws {
        let suiteName = "TermsAcceptanceStoreTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        XCTAssertFalse(TermsAcceptanceStore.isCurrentVersionAccepted(defaults: defaults))
        TermsAcceptanceStore.acceptCurrentVersion(defaults: defaults)
        XCTAssertTrue(TermsAcceptanceStore.isCurrentVersionAccepted(defaults: defaults))
    }
}
