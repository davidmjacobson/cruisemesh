import SwiftUI
import XCTest
@testable import CruiseMesh

final class AppearancePreferenceTests: XCTestCase {
    func testMissingAndUnrecognizedValuesFollowTheSystem() {
        XCTAssertEqual(AppearancePreference(storedValue: nil), .system)
        XCTAssertEqual(AppearancePreference(storedValue: "sepia"), .system)
    }

    func testStoredValuesRoundTrip() {
        for preference in AppearancePreference.allCases {
            XCTAssertEqual(
                AppearancePreference(storedValue: preference.rawValue),
                preference
            )
        }
    }

    func testThemeChoicesResolveToColorSchemes() {
        XCTAssertNil(AppearancePreference.system.colorScheme)
        XCTAssertEqual(AppearancePreference.light.colorScheme, .light)
        XCTAssertEqual(AppearancePreference.dark.colorScheme, .dark)
    }
}
