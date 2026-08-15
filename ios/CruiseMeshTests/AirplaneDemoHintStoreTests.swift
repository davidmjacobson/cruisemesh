import XCTest
@testable import CruiseMesh

final class AirplaneDemoHintStoreTests: XCTestCase {
    func testHintIsOfferedOnceAndNeverAgain() throws {
        let suiteName = "AirplaneDemoHintStoreTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        XCTAssertTrue(AirplaneDemoHintStore.shouldShow(defaults: defaults))
        AirplaneDemoHintStore.markShown(defaults: defaults)
        XCTAssertFalse(AirplaneDemoHintStore.shouldShow(defaults: defaults))
        AirplaneDemoHintStore.markShown(defaults: defaults)
        XCTAssertFalse(AirplaneDemoHintStore.shouldShow(defaults: defaults))
    }
}
