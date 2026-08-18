import Foundation

enum BackupService {
    private static var pendingDatabaseURL: URL {
        AppStore.databaseURL.appendingPathExtension("restore")
    }

    static let defaultContentOptions = BackupContentOptions(
        includeMessageHistory: true,
        includePendingDeliveriesForOthers: false
    )

    static func inventory(nowMs: Int64 = currentTimeMs) throws -> BackupInventory {
        try AppStore.get().backupInventory(nowMs: nowMs)
    }

    static func buildBackup(
        passphrase: String,
        options: BackupContentOptions = defaultContentOptions
    ) throws -> Data {
        guard let identity = IdentityStore.load() else {
            throw BackupServiceError.noIdentity
        }
        let snapshot = FileManager.default.temporaryDirectory
            .appendingPathComponent("cruisemesh-\(UUID().uuidString).sqlite")
        defer { try? FileManager.default.removeItem(at: snapshot) }
        let report = try AppStore.get().backupToWithOptions(
            destination: snapshot.path,
            options: options,
            nowMs: currentTimeMs
        )
        NSLog(
            "Prepared backup snapshot; removed messages=\(report.removedMessageCount) " +
                "ownPending=\(report.removedPendingOwnDeliveryCount) " +
                "courier=\(report.removedCourierDeliveryCount) " +
                "expired=\(report.removedExpiredDeliveryCount) " +
                "connectionEvents=\(report.removedConnectionEventCount)"
        )
        let sqlite = try Data(contentsOf: snapshot)
        let relay = RelayConfigStore.load()
        let payload = CoreBackupPayload(
            identity: encodeIdentityBytes(identity: identity),
            sqlite: sqlite,
            srcVersionCode: appVersionCode,
            createdAtMs: Int64(Date().timeIntervalSince1970 * 1_000),
            displayName: ProfileStore.loadDisplayName(),
            ownAvatar: ProfilePhotoStore.loadBackupBytes(),
            ownAvatarEpoch: ProfileStore.loadOwnAvatarEpoch(),
            relayUrl: relay?.relayUrl,
            relayToken: relay?.relayToken,
            shareOnline: RelayConfigStore.shareOnline(),
            friendsOfFriendsEnabled: FriendsOfFriendsStore.isEnabled()
        )
        return try sealBackup(passphrase: passphrase, payload: payload, iterations: nil)
    }

    static func previewBackup(file: Data, passphrase: String) throws -> BackupPreview {
        let payload = try openAndValidatePayload(file: file, passphrase: passphrase)
        let inventory = try withTemporaryDatabase(payload.sqlite) { staged in
            try inspectRestoredMessageStore(path: staged.path, nowMs: currentTimeMs)
        }
        return BackupPreview(
            inventory: inventory,
            createdAtMs: payload.createdAtMs,
            sourceVersionCode: payload.srcVersionCode,
            // §9's closing paragraph: opening a `.cmbak` on a fresh install is two
            // different intents wearing one word, and the person has to be the one
            // who picks. Read on the decrypt this screen already performs, so
            // choosing does not cost a second passphrase entry.
            //
            // The list is core's, order included — "Link as new device" is
            // deliberately first there, and the surface must not reorder a choice
            // whose ordering is itself the recommendation.
            plans: restorePlans(payload: payload)
        )
    }

    static func stageRestore(
        file: Data,
        passphrase: String,
        options: BackupContentOptions = defaultContentOptions
    ) throws {
        let payload = try openAndValidatePayload(file: file, passphrase: passphrase)
        let identity = try decodeIdentityBytes(bytes: payload.identity)
        try FileManager.default.createDirectory(
            at: pendingDatabaseURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try stageSanitizedDatabase(payload.sqlite, options: options)
        IdentityStore.save(identity)
        if let name = payload.displayName { ProfileStore.saveDisplayName(name) }
        ProfilePhotoStore.restoreBackupBytes(payload.ownAvatar)
        ProfileStore.restoreOwnAvatarEpoch(payload.ownAvatarEpoch)
        if let url = payload.relayUrl, let token = payload.relayToken {
            RelayConfigStore.save(relayUrl: url, relayToken: token)
        } else {
            RelayConfigStore.save(relayUrl: "", relayToken: "")
        }
        RelayConfigStore.setShareOnline(payload.shareOnline)
        _ = FriendsOfFriendsStore.setEnabled(payload.friendsOfFriendsEnabled)
        OnboardingStore.markCompleted()
    }

    /// Validate/migrate the payload and remove restored courier/relay runtime
    /// state before placing it at the special path startup knows how to install.
    /// A failed sanitization never leaves an unsafe pending restore behind.
    ///
    /// Mirrors the Android restore path: sanitize on a temp file, then place it
    /// at the pending path without re-materializing the whole DB as `Data`.
    /// Holding the encrypted backup + decrypted sqlite already peaks memory;
    /// `Data(contentsOf:)` of an ~88 MiB store was a third full copy (Android
    /// OOM on Pixel 10 under a 256 MiB heap).
    private static func stageSanitizedDatabase(
        _ sqlite: Data,
        options: BackupContentOptions
    ) throws {
        guard !sqlite.isEmpty else {
            try sqlite.write(to: pendingDatabaseURL, options: .atomic)
            return
        }
        let manager = FileManager.default
        let staged = manager.temporaryDirectory
            .appendingPathComponent("cruisemesh-restore-\(UUID().uuidString).sqlite")
        var movedToDestination = false
        defer {
            for suffix in ["-journal", "-wal", "-shm"] {
                try? manager.removeItem(at: URL(fileURLWithPath: staged.path + suffix))
            }
            if !movedToDestination {
                try? manager.removeItem(at: staged)
            }
        }
        try sqlite.write(to: staged, options: .atomic)
        let report = try sanitizeRestoredMessageStoreWithOptions(
            path: staged.path,
            options: options,
            nowMs: currentTimeMs
        )
        NSLog(
            "Sanitized restored store; removed messages=\(report.removedMessageCount) " +
                "ownPending=\(report.removedPendingOwnDeliveryCount) " +
                "courier=\(report.removedCourierDeliveryCount) " +
                "expired=\(report.removedExpiredDeliveryCount) " +
                "connectionEvents=\(report.removedConnectionEventCount)"
        )
        try relocateStagedDatabase(from: staged, to: pendingDatabaseURL, fileManager: manager)
        movedToDestination = true
    }

    /// Places a staged SQLite file at the pending restore path without loading
    /// the whole DB into a third `Data` buffer. Prefer rename; fall back to
    /// file-to-file copy if move is refused. On success the staged URL is
    /// always consumed (removed after a copy fallback).
    ///
    /// Internal (not private) so unit tests can drive the same path restore
    /// uses after sanitize — the step that used to OOM on Android via
    /// `Data(contentsOf:)`.
    static func relocateStagedDatabase(
        from staged: URL,
        to destination: URL,
        fileManager: FileManager = .default
    ) throws {
        if fileManager.fileExists(atPath: destination.path) {
            try fileManager.removeItem(at: destination)
        }
        do {
            try fileManager.moveItem(at: staged, to: destination)
        } catch {
            try fileManager.copyItem(at: staged, to: destination)
            try? fileManager.removeItem(at: staged)
        }
    }

    private static func openAndValidatePayload(
        file: Data,
        passphrase: String
    ) throws -> CoreBackupPayload {
        let payload = try openBackup(passphrase: passphrase, file: file)
        guard payload.srcVersionCode <= appVersionCode else {
            throw BackupServiceError.newerBackup(payload.srcVersionCode)
        }
        return payload
    }

    private static func withTemporaryDatabase<T>(
        _ sqlite: Data,
        operation: (URL) throws -> T
    ) throws -> T {
        let manager = FileManager.default
        let staged = manager.temporaryDirectory
            .appendingPathComponent("cruisemesh-restore-\(UUID().uuidString).sqlite")
        defer {
            for suffix in ["", "-journal", "-wal", "-shm"] {
                try? manager.removeItem(at: URL(fileURLWithPath: staged.path + suffix))
            }
        }
        try sqlite.write(to: staged, options: .atomic)
        return try operation(staged)
    }

    /// Read a selected backup incrementally so a malicious file provider cannot
    /// make the app accumulate an unbounded `Data` value before core validation.
    static func readBackupFile(
        at url: URL,
        maxBytes: Int = Int(backupMaxFileBytes())
    ) throws -> Data {
        precondition(maxBytes >= 0)
        let values = try url.resourceValues(forKeys: [.fileSizeKey])
        if let fileSize = values.fileSize, fileSize > maxBytes {
            throw BackupServiceError.fileTooLarge
        }
        guard let input = InputStream(url: url) else {
            throw CocoaError(.fileReadUnknown)
        }
        input.open()
        defer { input.close() }

        let chunkSize = 64 * 1024
        var buffer = [UInt8](repeating: 0, count: chunkSize)
        var result = Data()
        result.reserveCapacity(min(maxBytes, chunkSize))
        while true {
            let count = buffer.withUnsafeMutableBytes { rawBuffer -> Int in
                guard let baseAddress = rawBuffer.baseAddress else { return 0 }
                return input.read(
                    baseAddress.assumingMemoryBound(to: UInt8.self),
                    maxLength: chunkSize
                )
            }
            if count < 0 {
                throw input.streamError ?? CocoaError(.fileReadUnknown)
            }
            if count == 0 { break }
            guard count <= maxBytes - result.count else {
                throw BackupServiceError.fileTooLarge
            }
            result.append(contentsOf: buffer.prefix(count))
        }
        return result
    }

    /// Called before `AppStore` or `MeshController` is initialized, so the
    /// existing SQLite connection can never observe a file replacement.
    static func installPendingRestoreIfNeeded() throws {
        guard FileManager.default.fileExists(atPath: pendingDatabaseURL.path) else { return }
        let manager = FileManager.default
        let destination = AppStore.databaseURL
        try manager.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
        for suffix in ["-journal", "-wal", "-shm"] {
            let sibling = URL(fileURLWithPath: destination.path + suffix)
            if manager.fileExists(atPath: sibling.path) { try manager.removeItem(at: sibling) }
        }
        if manager.fileExists(atPath: destination.path) { try manager.removeItem(at: destination) }
        // Empty-store restore writes a zero-length pending file. Check size
        // only — do not load the pending DB into memory just to test emptiness.
        let size = try manager.attributesOfItem(atPath: pendingDatabaseURL.path)[.size] as? NSNumber
        if let size, size.intValue > 0 {
            try manager.moveItem(at: pendingDatabaseURL, to: destination)
        } else {
            try manager.removeItem(at: pendingDatabaseURL)
        }
    }

    static var suggestedFileName: String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyyMMdd-HHmm"
        return "cruisemesh-backup-\(formatter.string(from: Date())).cmbak"
    }

    private static var appVersionCode: Int32 {
        Int32(Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "0") ?? 0
    }

    private static var currentTimeMs: Int64 {
        Int64(Date().timeIntervalSince1970 * 1_000)
    }
}

struct BackupPreview: Equatable {
    let inventory: BackupInventory
    let createdAtMs: Int64
    let sourceVersionCode: Int32
    /// §9's "Replace this device" and "Link as new device", in core's order.
    /// Empty is never expected — `core_backup_restore_plans` always returns both
    /// — but a shell that finds it empty falls back to the old single-meaning
    /// restore rather than showing a fork with no branches.
    var plans: [CoreRestorePlan] = []
}

/// §9's two intents, or the one that keeps a restore working if core cannot
/// produce them.
///
/// `core_backup_restore_plans` always returns both, so the fallback is not an
/// expected state. It exists because the alternative — an empty list — silently
/// removes every branch from the fork, leaving a person who opened a backup with
/// a screen that offers nothing and a Restore button that never enables. Seeding
/// the replace intent degrades to exactly the behaviour restore had before the
/// fork existed, which is the honest floor: it is the branch that finishes here,
/// and the one whose hazard the copy already warns about.
private func restorePlans(payload: CoreBackupPayload) -> [CoreRestorePlan] {
    if let plans = try? coreBackupRestorePlans(payload: payload), !plans.isEmpty {
        return plans
    }
    // The person id comes out of the identity block the backup already carries;
    // with neither that nor core's plans there is genuinely nothing to offer.
    guard let identity = try? decodeIdentityBytes(bytes: payload.identity) else { return [] }
    return [
        CoreRestorePlan(
            intent: .replaceThisDevice,
            personId: identity.userId,
            restoresStoredHistory: true,
            keepsExistingDeviceIdentity: true,
            mintsNewDeviceKey: false,
            routesToLinkCeremony: false,
            carriesRecoveryMaterial: false,
            cloneHazardIfSourceIsLive: true
        )
    ]
}

enum BackupServiceError: LocalizedError, Equatable {
    case noIdentity
    case newerBackup(Int32)
    case fileTooLarge

    var errorDescription: String? {
        // One source of copy: the same sentences the backup screens show.
        switch self {
        case .noIdentity: return BackupFailureReason.noAccountToBackUp.text
        case .newerBackup: return BackupFailureReason.newerVersion.text
        case .fileTooLarge: return BackupFailureReason.tooLarge.text
        }
    }
}
