import Foundation
import OSLog

/// T13: opt-in iOS diagnostic-log archive. The OS already retains this app's
/// current-process Logger entries, so enabling capture has no continuous
/// reader or battery cost. When the app backgrounds (and immediately before a
/// share), we copy new entries into an app-private bounded file. That makes a
/// cruise tester's log survive process termination and later internet access.
///
/// Metadata only, by construction: every CruiseMesh log site logs routes,
/// addresses, counts, lamports, and contact/group *names* -- never message
/// text or payloads (audited) -- and any value marked private is redacted to
/// `<private>` in `composedMessage` regardless.
enum DiagnosticLogExport {
    private static let subsystem = "com.cruisemesh"
    private static let enabledKey = "diagnostic_log_export_enabled"
    private static let lastArchivedAtKey = "diagnostic_log_export_last_archived_at"
    private static let directoryName = "Diagnostics"
    private static let fileName = "cruisemesh-diagnostics.txt"
    private static let lock = NSLock()

    /// Bound each unified-log read and the persistent archive.
    private static let window: TimeInterval = 6 * 60 * 60
    private static let maxEntries = 5_000
    private static let maxArchiveBytes = 4 * 1024 * 1024

    static var isEnabled: Bool {
        UserDefaults.standard.bool(forKey: enabledKey)
    }

    static func setEnabled(_ enabled: Bool) {
        if !enabled {
            // Capture everything up to the moment the tester turns logging off.
            archiveCurrentSession(force: true)
        }
        UserDefaults.standard.set(enabled, forKey: enabledKey)
        if enabled {
            // Include useful events that occurred earlier in this launch before
            // the tester reached Connection details and enabled the switch.
            archiveCurrentSession(force: true)
        }
    }

    /// Called from the app lifecycle as the scene leaves the foreground.
    static func archiveCurrentSession() {
        archiveCurrentSession(force: false)
    }

    /// Flushes the current session and returns the persistent shareable archive,
    /// or `nil` when no CruiseMesh diagnostics have ever been captured.
    static func writeLogFile() -> URL? {
        archiveCurrentSession(force: true)
        guard let url = archiveURL(),
              let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              let size = attributes[.size] as? NSNumber,
              size.intValue > 0 else {
            return nil
        }
        return url
    }

    private static func archiveCurrentSession(force: Bool) {
        guard force || isEnabled else { return }
        lock.lock()
        defer { lock.unlock() }

        guard let store = try? OSLogStore(scope: .currentProcessIdentifier) else { return }
        let defaults = UserDefaults.standard
        let lastArchivedAt = defaults.object(forKey: lastArchivedAtKey) as? Date
        let windowStart = Date().addingTimeInterval(-window)
        let start = max(lastArchivedAt ?? windowStart, windowStart)
        let position = store.position(date: start)
        guard let entries = try? store.getEntries(at: position) else { return }

        let stamp = ISO8601DateFormatter()
        var records: [(date: Date, line: String)] = []
        for entry in entries {
            guard let log = entry as? OSLogEntryLog, log.subsystem == subsystem else {
                continue
            }
            if let lastArchivedAt, entry.date <= lastArchivedAt { continue }
            records.append(
                (
                    entry.date,
                    "\(stamp.string(from: entry.date)) [\(log.category)] \(levelLabel(log.level)) \(entry.composedMessage)"
                )
            )
        }
        guard !records.isEmpty, let url = archiveURL() else { return }
        if records.count > maxEntries { records = Array(records.suffix(maxEntries)) }

        var text = records.map(\.line).joined(separator: "\n") + "\n"
        if !FileManager.default.fileExists(atPath: url.path) {
            text = "CruiseMesh diagnostics — opt-in archive (metadata only)\n\n" + text
        }
        do {
            let data = Data(text.utf8)
            if FileManager.default.fileExists(atPath: url.path) {
                let handle = try FileHandle(forWritingTo: url)
                try handle.seekToEnd()
                try handle.write(contentsOf: data)
                try handle.close()
            } else {
                try data.write(to: url, options: .atomic)
            }
            if let newestDate = records.last?.date {
                defaults.set(newestDate, forKey: lastArchivedAtKey)
            }
            trimArchiveIfNeeded(url)
        } catch {
            return
        }
    }

    /// Whether any archive exists to share or delete.
    static func hasArchive() -> Bool {
        guard let url = archiveURL(),
              let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              let size = attributes[.size] as? NSNumber else {
            return false
        }
        return size.intValue > 0
    }

    /// Erases the persistent archive.
    ///
    /// The archive survives app restarts by design and its entries include
    /// contact and group *names*, so turning capture off has to be separable
    /// from erasing what was already captured. `lastArchivedAt` is moved to
    /// now rather than cleared: clearing it would let the next flush re-read
    /// the unified log back to the start of the window and rewrite the very
    /// entries the user just deleted.
    static func deleteArchive() {
        lock.lock()
        defer { lock.unlock() }
        if let url = archiveURL() {
            try? FileManager.default.removeItem(at: url)
        }
        UserDefaults.standard.set(Date(), forKey: lastArchivedAtKey)
    }

    private static func archiveURL() -> URL? {
        guard let base = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        let directory = base.appendingPathComponent(directoryName, isDirectory: true)
        do {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true
            )
        } catch {
            return nil
        }
        return directory.appendingPathComponent(fileName)
    }

    private static func trimArchiveIfNeeded(_ url: URL) {
        guard let data = try? Data(contentsOf: url),
              data.count > maxArchiveBytes else {
            return
        }
        var suffix = Data(data.suffix(maxArchiveBytes))
        if let newline = suffix.firstIndex(of: 0x0A) {
            suffix = Data(suffix.suffix(from: suffix.index(after: newline)))
        }
        let header = Data(
            "CruiseMesh diagnostics — opt-in archive (metadata only; oldest entries trimmed)\n\n".utf8
        )
        var trimmed = header
        trimmed.append(suffix)
        try? trimmed.write(to: url, options: .atomic)
    }

    /// Directory `MetricKitCollector` writes its
    /// JSON payloads into, and where `metricKitFileURLs()` below reads them
    /// back from for "Share diagnostics" to attach alongside the log file.
    /// Creates the directory on first use if it doesn't exist yet.
    static func metricKitDirectory() -> URL? {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(
            "cruisemesh-metrickit",
            isDirectory: true
        )
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        } catch {
            return nil
        }
        return dir
    }

    /// Existing `MetricKitCollector` JSON payloads, oldest first (filenames
    /// are timestamp-ordered), for "Share diagnostics" to attach alongside
    /// the log file. An empty result means nothing to attach, not an error --
    /// MetricKit may not have delivered a payload yet this install.
    static func metricKitFileURLs() -> [URL] {
        guard let dir = metricKitDirectory() else { return [] }
        let files = (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? []
        return files.sorted { $0.lastPathComponent < $1.lastPathComponent }
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
