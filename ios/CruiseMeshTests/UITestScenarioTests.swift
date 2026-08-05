import XCTest
@testable import CruiseMesh

final class UITestScenarioTests: XCTestCase {
    func testRootStateForEveryDeterministicScenario() {
        XCTAssertFalse(UITestConfiguration.Scenario.terms.termsAccepted)
        XCTAssertFalse(UITestConfiguration.Scenario.terms.onboardingCompleted)

        XCTAssertTrue(UITestConfiguration.Scenario.onboarding.termsAccepted)
        XCTAssertFalse(UITestConfiguration.Scenario.onboarding.onboardingCompleted)

        for scenario in [UITestConfiguration.Scenario.homeEmpty, .chat] {
            XCTAssertTrue(scenario.termsAccepted)
            XCTAssertTrue(scenario.onboardingCompleted)
        }
    }

    func testUnknownScenarioCannotSilentlyCreateAProductionState() {
        XCTAssertNil(UITestConfiguration.Scenario(rawValue: "unknown"))
    }
}
