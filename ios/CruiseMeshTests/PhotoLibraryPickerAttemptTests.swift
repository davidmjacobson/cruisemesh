import XCTest
@testable import CruiseMesh

final class PhotoLibraryPickerAttemptTests: XCTestCase {
    func testDismissalWithoutASelectionIsCancellation() {
        var attempt = PhotoLibraryPickerAttempt()

        attempt.begin()

        XCTAssertEqual(attempt.dismissal, .cancelled)
    }

    func testSelectionSurvivesBindingCleanupForDismissalDiagnostics() {
        var attempt = PhotoLibraryPickerAttempt()
        attempt.begin()

        attempt.selected()

        XCTAssertEqual(attempt.dismissal, .selected)
    }

    func testNewAttemptClearsPreviousSelection() {
        var attempt = PhotoLibraryPickerAttempt()
        attempt.selected()

        attempt.begin()

        XCTAssertEqual(attempt.dismissal, .cancelled)
    }
}
