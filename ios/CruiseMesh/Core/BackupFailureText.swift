import Foundation

/// What to put on screen when a backup or restore fails.
///
/// Every failure the backup screens can hit arrives as a typed error whose own
/// description is written for a developer, not a person: the core's backup
/// errors describe themselves by reflection, so a wrong passphrase reaches the
/// screen as `CoreBackupError.WrongPassphraseOrCorrupt`. Each known
/// failure is therefore mapped here to a real sentence, and anything
/// unrecognised uses its own description only when that description is
/// actually non-blank and not one of those reflected type names.
enum BackupFailureReason: Equatable, CaseIterable {
    case wrongPassphraseOrDamaged
    case notACruiseMeshBackup
    case incompleteFile
    case newerVersion
    case unreadableBackup
    case tooLarge
    case noAccountToBackUp
    case couldNotSave
    case couldNotRestore
    case couldNotReadFile

    /// The sentence a person reads.
    var text: String {
        switch self {
        case .wrongPassphraseOrDamaged:
            return String(localized: "That passphrase didn't work, or the file is damaged. Check the passphrase and try again.")
        case .notACruiseMeshBackup:
            return String(localized: "That file isn't a CruiseMesh backup. Pick the file you saved from CruiseMesh.")
        case .incompleteFile:
            return String(localized: "That backup file is incomplete. It may not have finished copying — try the original file again.")
        case .newerVersion:
            return String(localized: "This backup was made by a newer version of CruiseMesh. Update the app, then try again.")
        case .unreadableBackup:
            return String(localized: "That backup couldn't be read. The file may be damaged.")
        case .tooLarge:
            return String(localized: "This backup file is too large.")
        case .noAccountToBackUp:
            return String(localized: "There's no account on this phone to back up yet.")
        case .couldNotSave:
            return String(localized: "The backup couldn't be saved. Try again.")
        case .couldNotRestore:
            return String(localized: "That backup couldn't be restored. Try again.")
        case .couldNotReadFile:
            return String(localized: "That file couldn't be read. Pick it again.")
        }
    }
}

/// Either a sentence this mapping chose, or a message that already arrived
/// written for the user (a file-system error, say).
enum BackupFailureText: Equatable {
    case reason(BackupFailureReason)
    case literal(String)

    var text: String {
        switch self {
        case .reason(let reason): return reason.text
        case .literal(let message): return message
        }
    }
}

/// A description like `CoreBackupError.BadMagic` — or
/// `CruiseMesh.CoreError.Store("disk full")` — is a generated error reflecting
/// its own type name rather than describing itself. Never show one. A real
/// sentence's first word never contains a dot; a reflected type path always
/// does, whatever module qualifier the compiler puts in front of it.
func backupFailureDescriptionLooksLikeATypeName(_ description: String) -> Bool {
    let head = description.prefix { !$0.isWhitespace && $0 != "(" }
    guard head.contains(".") else { return false }
    return head.allSatisfy { $0.isLetter || $0.isNumber || $0 == "_" || $0 == "." }
}

/// Map a thrown [error] to the sentence the user should read.
/// [fallback] covers unexpected failures that carry no usable message, and
/// differs per screen (saving, restoring, picking a file).
func backupFailureText(_ error: Error, fallback: BackupFailureReason) -> BackupFailureText {
    if let core = error as? CoreBackupError {
        switch core {
        case .WrongPassphraseOrCorrupt:
            return .reason(.wrongPassphraseOrDamaged)
        case .BadMagic:
            return .reason(.notACruiseMeshBackup)
        case .Truncated:
            return .reason(.incompleteFile)
        case .UnsupportedVersion, .UnsupportedKdf:
            return .reason(.newerVersion)
        case .InvalidPayload:
            return .reason(.unreadableBackup)
        }
    }
    if let service = error as? BackupServiceError {
        switch service {
        case .noIdentity:
            return .reason(.noAccountToBackUp)
        case .newerBackup:
            return .reason(.newerVersion)
        case .fileTooLarge:
            return .reason(.tooLarge)
        }
    }
    let description = error.localizedDescription
    guard !description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
          !backupFailureDescriptionLooksLikeATypeName(description) else {
        return .reason(fallback)
    }
    return .literal(description)
}
