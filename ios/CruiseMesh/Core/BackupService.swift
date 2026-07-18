import Foundation

enum BackupService {
    private static var pendingDatabaseURL: URL {
        AppStore.databaseURL.appendingPathExtension("restore")
    }

    static func buildBackup(passphrase: String) throws -> Data {
        guard let identity = IdentityStore.load() else {
            throw BackupServiceError.noIdentity
        }
        let snapshot = FileManager.default.temporaryDirectory
            .appendingPathComponent("cruisemesh-\(UUID().uuidString).sqlite")
        defer { try? FileManager.default.removeItem(at: snapshot) }
        try AppStore.get().backupTo(destination: snapshot.path)
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
            shareOnline: RelayConfigStore.shareOnline()
        )
        return try sealBackup(passphrase: passphrase, payload: payload, iterations: nil)
    }

    static func stageRestore(file: Data, passphrase: String) throws {
        let payload = try openBackup(passphrase: passphrase, file: file)
        guard payload.srcVersionCode <= appVersionCode else {
            throw BackupServiceError.newerBackup(payload.srcVersionCode)
        }
        let identity = try decodeIdentityBytes(bytes: payload.identity)
        try FileManager.default.createDirectory(
            at: pendingDatabaseURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try payload.sqlite.write(to: pendingDatabaseURL, options: .atomic)
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
        OnboardingStore.markCompleted()
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
}

enum BackupServiceError: LocalizedError {
    case noIdentity
    case newerBackup(Int32)

    var errorDescription: String? {
        switch self {
        case .noIdentity: return "No identity is available to back up."
        case .newerBackup: return "This backup was created by a newer version of CruiseMesh. Update the app first."
        }
    }
}
