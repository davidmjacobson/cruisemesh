import Foundation
import os.log

/// Single on-device `MessageStore` (messages + contacts), same role as Android `AppStore`.
enum AppStore {
    private static var cached: MessageStore?
    private static let lock = NSLock()
    private static let log = Logger(subsystem: "com.cruisemesh", category: "AppStore")

    /// Set when `get()` had to recover from a corrupt/unopenable database by
    /// moving the bad file aside and starting fresh (FI9). Consumed once via
    /// `consumeRecoveryNotice()` so a caller (`ChatListView`) can surface it.
    private static var recoveryNoticePending = false

    static var databaseURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("CruiseMesh", isDirectory: true)
            .appendingPathComponent("messages.sqlite")
    }

    static func get() -> MessageStore {
        lock.lock()
        defer { lock.unlock() }
        if let cached { return cached }
        let dir = databaseURL.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let path = databaseURL.path
        let store: MessageStore
        do {
            store = try MessageStore.open(path: path)
        } catch {
            // A corrupt/unopenable DB used to be a crash loop at launch
            // (FI9). Move the bad file aside (with its -wal/-shm siblings)
            // and start fresh rather than crashing every time the app opens.
            log.error("MessageStore.open failed: \(error.localizedDescription, privacy: .public); moving database aside and starting fresh")
            moveAsideUnopenableDatabase(path: path)
            store = try! MessageStore.open(path: path)
            recoveryNoticePending = true
        }
        cached = store
        return store
    }

    /// True at most once per recovery — callers should surface it and it
    /// won't fire again until another recovery happens.
    static func consumeRecoveryNotice() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        defer { recoveryNoticePending = false }
        return recoveryNoticePending
    }

    private static func moveAsideUnopenableDatabase(path: String) {
        let fm = FileManager.default
        let suffix = "corrupt-\(Int(Date().timeIntervalSince1970))"
        for ext in ["", "-wal", "-shm"] {
            let src = path + ext
            guard fm.fileExists(atPath: src) else { continue }
            let dst = path + "." + suffix + ext
            try? fm.removeItem(atPath: dst)
            try? fm.moveItem(atPath: src, toPath: dst)
        }
    }
}
