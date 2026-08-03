import XCTest
@testable import CruiseMesh

private struct BlankDescriptionError: LocalizedError {
    let description: String
    var errorDescription: String? { description }
}

final class BackupFailureTextTests: XCTestCase {
    private let fallback = BackupFailureReason.couldNotRestore

    func testKnownCoreFailuresGetASentenceInsteadOfATypeName() {
        // The trap this guards: the generated bindings describe themselves by
        // reflection, so localizedDescription reads like
        // "CoreBackupError.WrongPassphraseOrCorrupt" — a type path, not a
        // sentence — and that is what used to reach the screen.
        let raw = CoreBackupError.WrongPassphraseOrCorrupt.localizedDescription
        XCTAssertTrue(raw.contains("CoreBackupError"))
        XCTAssertTrue(backupFailureDescriptionLooksLikeATypeName(raw))
        XCTAssertFalse(backupFailureDescriptionLooksLikeATypeName("The file could not be opened."))

        XCTAssertEqual(
            backupFailureText(CoreBackupError.WrongPassphraseOrCorrupt, fallback: fallback),
            .reason(.wrongPassphraseOrDamaged)
        )
        XCTAssertEqual(
            backupFailureText(CoreBackupError.BadMagic, fallback: fallback),
            .reason(.notACruiseMeshBackup)
        )
        XCTAssertEqual(
            backupFailureText(CoreBackupError.Truncated, fallback: fallback),
            .reason(.incompleteFile)
        )
        XCTAssertEqual(
            backupFailureText(CoreBackupError.UnsupportedVersion(version: 9), fallback: fallback),
            .reason(.newerVersion)
        )
        XCTAssertEqual(
            backupFailureText(CoreBackupError.UnsupportedKdf(kdfId: 7), fallback: fallback),
            .reason(.newerVersion)
        )
        XCTAssertEqual(
            backupFailureText(CoreBackupError.InvalidPayload(reason: "short"), fallback: fallback),
            .reason(.unreadableBackup)
        )
    }

    func testAppLevelBackupFailuresGetASentence() {
        XCTAssertEqual(
            backupFailureText(BackupServiceError.noIdentity, fallback: fallback),
            .reason(.noAccountToBackUp)
        )
        XCTAssertEqual(
            backupFailureText(BackupServiceError.newerBackup(2), fallback: fallback),
            .reason(.newerVersion)
        )
        XCTAssertEqual(
            backupFailureText(BackupServiceError.fileTooLarge, fallback: fallback),
            .reason(.tooLarge)
        )
    }

    func testUnknownErrorsKeepARealMessageAndFallBackOnABlankOne() {
        XCTAssertEqual(
            backupFailureText(
                BlankDescriptionError(description: "The file could not be opened."),
                fallback: fallback
            ),
            .literal("The file could not be opened.")
        )
        XCTAssertEqual(
            backupFailureText(BlankDescriptionError(description: ""), fallback: fallback),
            .reason(fallback)
        )
        XCTAssertEqual(
            backupFailureText(BlankDescriptionError(description: "   "), fallback: fallback),
            .reason(fallback)
        )
    }

    func testUnknownCoreErrorsNeverLeakTheirTypeName() {
        // Any other generated error describes itself the same reflected way.
        XCTAssertEqual(
            backupFailureText(CoreError.SignatureInvalid, fallback: .couldNotReadFile),
            .reason(.couldNotReadFile)
        )
    }

    func testEveryReasonHasNonEmptyCopy() {
        for reason in BackupFailureReason.allCases {
            XCTAssertFalse(reason.text.trimmingCharacters(in: .whitespaces).isEmpty)
            XCTAssertFalse(backupFailureDescriptionLooksLikeATypeName(reason.text))
        }
    }

    func testTheFallbackDiffersPerScreen() {
        let blank = BlankDescriptionError(description: "")
        XCTAssertEqual(backupFailureText(blank, fallback: .couldNotSave), .reason(.couldNotSave))
        XCTAssertEqual(backupFailureText(blank, fallback: .couldNotReadFile), .reason(.couldNotReadFile))
        XCTAssertEqual(backupFailureText(blank, fallback: .couldNotRestore), .reason(.couldNotRestore))
    }
}
