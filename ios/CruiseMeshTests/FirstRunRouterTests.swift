import XCTest
@testable import CruiseMesh

/// The rule the three first-run doors share: every one of them shows the
/// permissions step exactly once, and none of them shows it twice.
///
/// Each test walks a door end to end — the store values a real route would have
/// written at each point — rather than asserting a single mapping, because the
/// bug this pins was never a wrong answer to one question. It was a route that
/// never asked. Mirrors Android's `FirstRunRouterTest`.
final class FirstRunRouterTests: XCTestCase {

    func testWizardCarriesItsOwnPermissionsStepAndDoesNotRepeatIt() {
        // A fresh install: nothing recorded at all.
        XCTAssertEqual(
            FirstRunRouter.destination(
                setupComplete: false,
                permissionsStepDone: nil,
                meshPermissionsGranted: false
            ),
            .wizard
        )
        // Finishing the wizard records both facts. Slide 4 was the step.
        XCTAssertEqual(
            FirstRunRouter.destination(
                setupComplete: true,
                permissionsStepDone: true,
                meshPermissionsGranted: false
            ),
            .home
        )
    }

    func testOwnDeviceLinkDoesNotSkipThePermissionsStep() {
        // `LinkAdoption` marks setup complete and the step pending, in one go.
        XCTAssertEqual(
            FirstRunRouter.destination(
                setupComplete: true,
                permissionsStepDone: false,
                meshPermissionsGranted: false
            ),
            .permissions
        )
        // And once through it, the app — not the step again.
        XCTAssertEqual(
            FirstRunRouter.destination(
                setupComplete: true,
                permissionsStepDone: true,
                meshPermissionsGranted: false
            ),
            .home
        )
    }

    func testBackupRestoreDoesNotSkipThePermissionsStepEither() {
        XCTAssertEqual(
            FirstRunRouter.destination(
                setupComplete: true,
                permissionsStepDone: false,
                meshPermissionsGranted: false
            ),
            .permissions
        )
    }

    func testAPhoneThatAlreadyHasThePermissionIsNotAskedAgain() {
        XCTAssertEqual(
            FirstRunRouter.destination(
                setupComplete: true,
                permissionsStepDone: false,
                meshPermissionsGranted: true
            ),
            .home
        )
    }

    func testAnInstallOlderThanTheFlagIsNeverPulledBackIntoSetup() {
        XCTAssertEqual(
            FirstRunRouter.destination(
                setupComplete: true,
                permissionsStepDone: nil,
                meshPermissionsGranted: false
            ),
            .home
        )
    }

    func testAnUnfinishedWizardIsTheWizardWhateverElseIsRecorded() {
        for stepDone in [nil, true, false] as [Bool?] {
            for granted in [true, false] {
                XCTAssertEqual(
                    FirstRunRouter.destination(
                        setupComplete: false,
                        permissionsStepDone: stepDone,
                        meshPermissionsGranted: granted
                    ),
                    .wizard,
                    "stepDone=\(String(describing: stepDone)) granted=\(granted)"
                )
            }
        }
    }
}
