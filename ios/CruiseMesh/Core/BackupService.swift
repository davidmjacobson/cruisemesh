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
            sourceVersionCode: payload.srcVersionCode
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
    private static func stageSanitizedDatabase(
        _ sqlite: Data,
        options: BackupContentOptions
    ) throws {
        guard !sqlite.isEmpty else {
            try sqlite.write(to: pendingDatabaseURL, options: .atomic)
            return
        }
        let manager = FileManager.default
        let staged = try withTemporaryDatabase(sqlite) { staged in
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
            return try Data(contentsOf: staged)
        }
        if manager.fileExists(atPath: pendingDatabaseURL.path) {
            try manager.removeItem(at: pendingDatabaseURL)
        }
        try staged.write(to: pendingDatabaseURL, options: .atomic)
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
        let bytes = try Data(contentsOf: pendingDatabaseURL)
        if !bytes.isEmpty {
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
