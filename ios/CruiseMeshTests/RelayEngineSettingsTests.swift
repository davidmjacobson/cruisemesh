import XCTest
@testable import CruiseMesh

/// The rollback switch, and the two things that matter about it: what it says
/// when nobody has touched it, and that a legacy default with the canary on is
/// the shipping state. Mirrors Android `RelayEngineSettingsTest`.
///
/// The Android suite's "never touches the message store" reflection test has no
/// Swift equivalent worth writing: `RelayEngineSettings` is an enum whose every
/// method signature is `RelayPassEngine`, `Bool` or `CoreRelayShadowSampler`,
/// so the compiler already forbids a `MessageStore` from appearing here. That it
/// lives in `AppDefaults` (a `UserDefaults`) and never in the store schema is
/// what makes C5's removal a matter of ceasing to read a few keys.
final class RelayEngineSettingsTests: XCTestCase {

    private static let keys = [
        "cruisemesh.relay.passEngineCore",
        "cruisemesh.relay.passEngineShadow",
        "cruisemesh.relay.passEngineShadowDay",
        "cruisemesh.relay.passEngineShadowCount",
        "cruisemesh.relay.passEngineShadowLastMs",
    ]

    override func setUp() {
        super.setUp()
        Self.keys.forEach { AppDefaults.current.removeObject(forKey: $0) }
    }

    override func tearDown() {
        Self.keys.forEach { AppDefaults.current.removeObject(forKey: $0) }
        super.tearDown()
    }

    func testADeviceThatHasNeverBeenToldAnythingRunsTheLegacyEngine() {
        // The default is the whole safety property of this package: with nothing
        // set, nothing about a relay pass changes.
        XCTAssertEqual(RelayEngineSettings.passEngine(), .legacy)
    }

    func testTheCanaryIsOnByDefault() {
        XCTAssertTrue(RelayEngineSettings.shadowEnabled())
    }

    func testTheSelectionRoundTripsAndCanBePutBack() {
        RelayEngineSettings.setPassEngine(.core)
        XCTAssertEqual(RelayEngineSettings.passEngine(), .core)
        // Rollback is a preference write, not a migration.
        RelayEngineSettings.setPassEngine(.legacy)
        XCTAssertEqual(RelayEngineSettings.passEngine(), .legacy)
    }

    func testTurningTheCanaryOffLeavesTheEngineAlone() {
        RelayEngineSettings.setShadowEnabled(false)
        XCTAssertFalse(RelayEngineSettings.shadowEnabled())
        XCTAssertEqual(RelayEngineSettings.passEngine(), .legacy)
        XCTAssertFalse(relayShadowPermitted(.legacy, shadowEnabled: RelayEngineSettings.shadowEnabled()))
    }

    func testTheSamplerRoundTripsSoTheDailyBoundSurvivesARestart() {
        let state = CoreRelayShadowSampler(dayIndex: 19_500, samplesToday: 7, lastSampleAtMs: 1_700_000_000_000)
        RelayEngineSettings.setShadowSampler(state)
        XCTAssertEqual(RelayEngineSettings.shadowSampler(), state)
    }

    func testShadowingIsRefusedWhenTheCoreEngineIsTheOneRunningThePass() {
        XCTAssertTrue(relayShadowPermitted(.legacy, shadowEnabled: true))
        XCTAssertFalse(relayShadowPermitted(.legacy, shadowEnabled: false))
        // Comparing the core planner against the core engine agrees every time,
        // which is indistinguishable from evidence and is not evidence.
        XCTAssertFalse(relayShadowPermitted(.core, shadowEnabled: true))
        XCTAssertFalse(relayShadowPermitted(.core, shadowEnabled: false))
    }
}
