import XCTest
@testable import CruiseMesh

final class TermsAcceptanceStoreTests: XCTestCase {
    func testAcceptanceIsVersioned() throws {
        let suiteName = "TermsAcceptanceStoreTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        XCTAssertFalse(TermsAcceptanceStore.isCurrentVersionAccepted(defaults: defaults))
        TermsAcceptanceStore.acceptCurrentVersion(defaults: defaults)
        XCTAssertTrue(TermsAcceptanceStore.isCurrentVersionAccepted(defaults: defaults))
    }
}
