import Foundation

/// Single on-device `MessageStore` (messages + contacts), same role as Android `AppStore`.
enum AppStore {
    private static var cached: MessageStore?
    private static let lock = NSLock()

    static func get() -> MessageStore {
        lock.lock()
        defer { lock.unlock() }
        if let cached { return cached }
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("CruiseMesh", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let path = dir.appendingPathComponent("messages.sqlite").path
        let store = try! MessageStore.open(path: path)
        cached = store
        return store
    }
}
