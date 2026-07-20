import Foundation
import OSLog

/// T13: on-demand iOS diagnostic-log capture. Reads this app's own os.Logger
/// entries for the current process (this session) from the unified log and
/// writes them to a shareable text file. Unlike Android there's no opt-in
/// switch or running capture cost -- the OS already retains the entries, so we
/// just read them when the user asks.
///
/// Metadata only, by construction: every CruiseMesh log site logs routes,
/// addresses, counts, lamports, and contact/group *names* -- never message
/// text or payloads (audited) -- and any value marked private is redacted to
/// `<private>` in `composedMessage` regardless.
enum DiagnosticLogExport {
    private static let subsystem = "com.cruisemesh"
    /// Bound the export so a long-lived session can't produce an enormous file.
    private static let window: TimeInterval = 6 * 60 * 60
    private static let maxEntries = 5_000

    /// Writes the recent diagnostic entries to a temp file and returns its URL,
    /// or `nil` when the store is unavailable or nothing has been logged yet.
    static func writeLogFile() -> URL? {
        guard let store = try? OSLogStore(scope: .currentProcessIdentifier) else { return nil }
        let position = store.position(date: Date().addingTimeInterval(-window))
        guard let entries = try? store.getEntries(at: position) else { return nil }

        let stamp = ISO8601DateFormatter()
        var lines: [String] = []
        for entry in entries {
            guard let log = entry as? OSLogEntryLog, log.subsystem == subsystem else { continue }
            lines.append(
                "\(stamp.string(from: entry.date)) [\(log.category)] \(levelLabel(log.level)) \(entry.composedMessage)"
            )
        }
        guard !lines.isEmpty else { return nil }
        if lines.count > maxEntries { lines = Array(lines.suffix(maxEntries)) }

        let text = "CruiseMesh diagnostics — current session (metadata only)\n\n"
            + lines.joined(separator: "\n") + "\n"
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("cruisemesh-diagnostics.txt")
        do {
            try text.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            return nil
        }
        return url
    }

    private static func levelLabel(_ level: OSLogEntryLog.Level) -> String {
        switch level {
        case .debug: return "DEBUG"
        case .info: return "INFO"
        case .notice: return "NOTICE"
        case .error: return "ERROR"
        case .fault: return "FAULT"
        case .undefined: return "-"
        @unknown default: return "-"
        }
    }
}
