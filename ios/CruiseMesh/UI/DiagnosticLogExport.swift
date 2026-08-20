import Foundation
import OSLog
import UIKit

/// Cancellation shared between the UIKit background-task expiry handler and
/// the diagnostics worker. `OSLogStore` iteration is lazy, so checking between
/// entries lets an expired job stop without advancing its persisted cursor;
/// the next lifecycle flush can safely retry the same entries.
final class DiagnosticArchiveCancellation: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false

    func cancel() {
        lock.lock()
        cancelled = true
        lock.unlock()
    }

    var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled
    }
}

/// Coalesces lifecycle archive requests and guarantees that the expensive
/// work runs on a utility queue rather than the caller's queue. Kept free of
/// UIKit so unit tests can prove that a scene callback never waits for the
/// worker and that expiry reaches it.
final class DiagnosticArchiveLifecycleScheduler: @unchecked Sendable {
    private let queue: DispatchQueue
    private let lock = NSLock()
    private var running = false

    init(queue: DispatchQueue) {
        self.queue = queue
    }

    @discardableResult
    func schedule(
        beginBackgroundTask: (@escaping () -> Void) -> () -> Void,
        work: @escaping (DiagnosticArchiveCancellation) -> Void
    ) -> Bool {
        lock.lock()
        guard !running else {
            lock.unlock()
            return false
        }
        running = true
        lock.unlock()

        let cancellation = DiagnosticArchiveCancellation()
        let endBackgroundTask = beginBackgroundTask { cancellation.cancel() }
        queue.async { [weak self] in
            work(cancellation)
            endBackgroundTask()
            self?.finish()
        }
        return true
    }

    private func finish() {
        lock.lock()
        running = false
        lock.unlock()
    }
}

/// Owns one UIKit background-task identifier. Expiry and normal completion
/// can race, so `end()` always hops to the main queue and is idempotent there.
private final class DiagnosticArchiveBackgroundTask: @unchecked Sendable {
    private var identifier = UIBackgroundTaskIdentifier.invalid

    func begin(expiration: @escaping () -> Void) {
        dispatchPrecondition(condition: .onQueue(.main))
        identifier = UIApplication.shared.beginBackgroundTask(
            withName: "CruiseMesh diagnostics archive"
        ) { [weak self] in
            expiration()
            self?.end()
        }
    }

    func end() {
        let endOnMain = { [weak self] in
            guard let self, self.identifier != .invalid else { return }
            UIApplication.shared.endBackgroundTask(self.identifier)
            self.identifier = .invalid
        }
        if Thread.isMainThread {
            endOnMain()
        } else {
            DispatchQueue.main.async(execute: endOnMain)
        }
    }
}

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
    private static let lifecycleScheduler = DiagnosticArchiveLifecycleScheduler(
        queue: DispatchQueue(label: "com.cruisemesh.diagnostics.archive", qos: .utility)
    )

    /// Guarded by `lock`, like every other mutable state in here.
    private static var sessionBannerWritten = false

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
    ///
    /// Never enumerate `OSLogStore` inline here. Apple treats a slow
    /// scene-update callback as a watchdog violation, and a relay failure
    /// storm can leave hundreds of entries for the lazy iterator to retrieve.
    /// The UIKit background task gives the utility worker time to finish after
    /// the scene backgrounds; its expiry handler asks the iterator to stop.
    static func archiveCurrentSession() {
        guard isEnabled else { return }
        let schedule = {
            _ = lifecycleScheduler.schedule(
                beginBackgroundTask: { expiration in
                    let task = DiagnosticArchiveBackgroundTask()
                    task.begin(expiration: expiration)
                    return { task.end() }
                },
                work: { cancellation in
                    archiveCurrentSession(force: false, cancellation: cancellation)
                }
            )
        }
        if Thread.isMainThread {
            schedule()
        } else {
            DispatchQueue.main.async(execute: schedule)
        }
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

    private static func archiveCurrentSession(
        force: Bool,
        cancellation: DiagnosticArchiveCancellation? = nil
    ) {
        guard force || isEnabled else { return }
        guard cancellation?.isCancelled != true else { return }
        lock.lock()
        defer { lock.unlock() }

        guard cancellation?.isCancelled != true else { return }
        guard let store = try? OSLogStore(scope: .currentProcessIdentifier) else { return }
        let defaults = UserDefaults.standard
        let lastArchivedAt = defaults.object(forKey: lastArchivedAtKey) as? Date
        let windowStart = Date().addingTimeInterval(-window)
        let start = max(lastArchivedAt ?? windowStart, windowStart)
        let position = store.position(date: start)
        // Ask the log store to filter before it hands entries to our lazy
        // iterator. Filtering `subsystem` in the loop below made the first
        // diagnostics share enumerate every framework log emitted by this
        // process, which can take many seconds even when the resulting
        // CruiseMesh archive is only a few kilobytes.
        let predicate = NSPredicate(format: "subsystem == %@", subsystem)
        guard let entries = try? store.getEntries(at: position, matching: predicate) else { return }

        let stamp = ISO8601DateFormatter()
        var records: [(date: Date, line: String)] = []
        for entry in entries {
            guard cancellation?.isCancelled != true else { return }
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
        guard cancellation?.isCancelled != true else { return }
        guard !records.isEmpty, let url = archiveURL() else { return }
        if records.count > maxEntries { records = Array(records.suffix(maxEntries)) }

        var text = records.map(\.line).joined(separator: "\n") + "\n"
        if !sessionBannerWritten {
            sessionBannerWritten = true
            text = sessionBanner() + text
        }
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

    /// Written once per launch, ahead of that launch's first batch of entries.
    ///
    /// Without it a shared archive is unattributable: the entries carry no app
    /// version, so a tester's log cannot be told apart from the same log on a
    /// build three releases newer, and the reader has no idea which binary the
    /// addresses in a crash report belong to. The archive spans launches and
    /// survives updates, so the banner has to repeat per launch rather than
    /// sit once at the top of the file.
    ///
    /// Metadata only, consistent with the rest of the archive: app version and
    /// build, hardware identifier, and OS version. The hardware identifier is
    /// the model (`iPhone14,2`), not any per-device serial.
    private static func sessionBanner() -> String {
        let bundle = Bundle.main
        let version = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
        let build = bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "?"
        let stamp = ISO8601DateFormatter().string(from: Date())
        return "\n===== launch \(stamp) — CruiseMesh \(version) (\(build)) — "
            + "\(hardwareIdentifier()) — \(ProcessInfo.processInfo.operatingSystemVersionString) =====\n"
            + EnvironmentSnapshot.line() + "\n"
    }

    /// `uname` machine string, e.g. `iPhone14,2`. `UIDevice.current.model`
    /// only ever returns "iPhone", which cannot distinguish the hardware a
    /// radio bug reproduces on.
    private static func hardwareIdentifier() -> String {
        var info = utsname()
        uname(&info)
        let machine = withUnsafeBytes(of: &info.machine) { raw in
            raw.prefix { $0 != 0 }
        }
        return String(decoding: machine, as: UTF8.self)
    }

    /// Whether anything exists to share or delete. Counts the MetricKit
    /// payloads too: they now outlive the process the way the log archive
    /// does, so a tester can have crash reports worth sharing -- and worth
    /// being able to erase -- on a launch that captured no log entries.
    static func hasArchive() -> Bool {
        if !metricKitFileURLs().isEmpty { return true }
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
    ///
    /// Also clears the MetricKit payloads. Before they moved to Application
    /// Support the OS eventually reclaimed them on its own; now that they
    /// persist, "delete captured diagnostics" is the only thing that removes
    /// them, and a delete that left crash reports behind would be a lie.
    static func deleteArchive() {
        lock.lock()
        defer { lock.unlock() }
        if let url = archiveURL() {
            try? FileManager.default.removeItem(at: url)
        }
        for url in metricKitFileURLs() {
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

    /// Directory `MetricKitCollector` writes its JSON payloads into -- both the
    /// daily metric summaries and the crash/hang diagnostics for previous
    /// launches -- and where `metricKitFileURLs()` below reads them back from
    /// for "Share diagnostics" to attach alongside the log file. Creates the
    /// directory on first use if it doesn't exist yet.
    ///
    /// Deliberately Application Support rather than `temporaryDirectory`: iOS
    /// may purge tmp whenever the app isn't running, and a crash report has to
    /// survive from the crash until whenever the tester next has a connection
    /// and gets around to sharing -- which on a cruise can be days. The daily
    /// metric payloads were tolerant of a purge; a crash report is the one
    /// artifact we cannot regenerate.
    static func metricKitDirectory() -> URL? {
        guard let base = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        let dir = base.appendingPathComponent(directoryName, isDirectory: true)
            .appendingPathComponent("MetricKit", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        } catch {
            return nil
        }
        return dir
    }

    /// The pre-move location. Read-only now, so payloads an updating tester
    /// already had on disk still get shared instead of silently disappearing
    /// on the release that moved the directory.
    private static func legacyMetricKitDirectory() -> URL {
        FileManager.default.temporaryDirectory.appendingPathComponent(
            "cruisemesh-metrickit",
            isDirectory: true
        )
    }

    /// Newest payloads to keep, per kind.
    ///
    /// Moving out of `temporaryDirectory` bought durability -- a crash report
    /// now survives until the tester has a connection -- but it also removed
    /// the only thing that ever bounded these files, since the OS used to
    /// reclaim tmp on its own. MetricKit collection is not gated by the
    /// diagnostics opt-in, so without a cap every install accumulates a JSON
    /// per day forever, next to a log archive that caps itself at 4 MB.
    ///
    /// Crashes get a larger allowance than the daily metric payloads: they are
    /// rarer, far more valuable, and a crash loop should not evict its own
    /// earliest evidence.
    private static let maxDiagnosticPayloads = 20
    private static let maxMetricPayloads = 14

    /// Trims each kind to its cap, newest kept. Filenames are ISO-8601 stamped
    /// so lexicographic order is chronological within a prefix.
    static func pruneMetricKitFiles() {
        guard let dir = metricKitDirectory() else { return }
        let files = (try? FileManager.default.contentsOfDirectory(
            at: dir,
            includingPropertiesForKeys: nil
        )) ?? []
        for (prefix, keep) in [
            ("diagnostic-", maxDiagnosticPayloads),
            ("metrickit-", maxMetricPayloads),
        ] {
            let matching = files
                .filter { $0.lastPathComponent.hasPrefix(prefix) }
                .sorted { $0.lastPathComponent < $1.lastPathComponent }
            guard matching.count > keep else { continue }
            for url in matching.prefix(matching.count - keep) {
                try? FileManager.default.removeItem(at: url)
            }
        }
    }

    /// Existing `MetricKitCollector` JSON payloads, oldest first (filenames
    /// are timestamp-ordered), for "Share diagnostics" to attach alongside
    /// the log file. An empty result means nothing to attach, not an error --
    /// MetricKit may not have delivered a payload yet this install.
    static func metricKitFileURLs() -> [URL] {
        var dirs: [URL] = [legacyMetricKitDirectory()]
        if let dir = metricKitDirectory() { dirs.append(dir) }
        let files = dirs.flatMap { dir in
            (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? []
        }
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
