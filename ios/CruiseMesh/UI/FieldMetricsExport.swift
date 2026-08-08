import SwiftUI
import UIKit

/// V2 field metrics: turns the core's delivery-metrics CSV into a shareable
/// file for the cruise test. Metadata only -- the CSV carries hashed chat tags,
/// lamports, transports, and timings, never message content or raw contact ids
/// (see the core `delivery_metrics` table).
enum FieldMetricsExport {
    /// Writes the current metrics to a temp CSV file, or `nil` when nothing has
    /// been captured yet (header row only).
    static func writeCSVFile() -> URL? {
        guard let csv = try? AppStore.get().exportDeliveryMetricsCsv() else { return nil }
        // A single line is the header with no data rows.
        let lines = csv.split(separator: "\n", omittingEmptySubsequences: true)
        guard lines.count > 1 else { return nil }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("cruisemesh-field-metrics.csv")
        do {
            try csv.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            return nil
        }
        return url
    }

    /// Whether any metrics rows exist. Drives whether "Delete captured
    /// diagnostics" has anything to act on.
    ///
    /// Asks the core rather than exporting: `EXISTS` stops at the first row,
    /// where the CSV export serialises every one of them -- thousands after a
    /// week aboard -- just to be counted and thrown away.
    static func hasCapturedMetrics() -> Bool {
        (try? AppStore.get().hasDeliveryMetrics()) ?? false
    }

    /// Removes the exported CSV written by `writeCSVFile`.
    ///
    /// Clearing the rows is not enough: the last export is a full copy of them
    /// sitting in the temporary directory, and "delete captured diagnostics"
    /// that left it there would be untrue until the OS happened to reclaim
    /// tmp. Mirrors Android's `deleteCsvFile`.
    static func deleteExportedCSV() {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("cruisemesh-field-metrics.csv")
        try? FileManager.default.removeItem(at: url)
    }
}

/// Metadata-only summaries of ambiguous incoming stream conflicts. Rust owns
/// both redaction and CSV formatting so neither mobile shell can accidentally
/// leak raw identities or quarantined message bodies.
enum ConflictDiagnosticsExport {
    private static let fileName = "cruisemesh-message-conflicts.csv"

    static func writeCSVFile() -> URL? {
        guard let csv = try? AppStore.get().exportMessageConflictsCsv() else { return nil }
        guard csv.split(separator: "\n", omittingEmptySubsequences: true).count > 1 else {
            return nil
        }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(fileName)
        do {
            try csv.write(to: url, atomically: true, encoding: .utf8)
            return url
        } catch {
            return nil
        }
    }

    static func hasCapturedConflicts() -> Bool {
        (try? AppStore.get().hasMessageConflicts()) ?? false
    }

    static func deleteExportedCSV() {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(fileName)
        try? FileManager.default.removeItem(at: url)
    }
}

/// The core's protocol-event ring, written into the shared diagnostics
/// archive.
///
/// Rust owns everything about the file: the schema, what may appear in a
/// record, the archive-local pseudonyms that stand in for contacts and
/// mailboxes, and the export itself. This type decides nothing -- it asks the
/// store for a string and puts it on disk. That is deliberate: the ring exists
/// so a support hand-off carries what the device actually decided, and a shell
/// that reformatted or filtered it on the way out would be one more place for
/// the two platforms to disagree. Android's `ProtocolEventExport` is the same
/// wrapper around the same call.
///
/// Nothing here uploads or schedules anything. The file is written only when
/// someone taps share.
enum ProtocolEventExport {
    private static let fileName = "cruisemesh-protocol-events.jsonl"

    /// A freshly exported ring, or `nil` when the core has nothing to say.
    static func writeJSONLFile() -> URL? {
        guard hasCapturedEvents() else { return nil }
        guard let jsonl = try? AppStore.get().exportProtocolEventsJsonl() else { return nil }
        // A header with no records answers nothing; its absence serves the
        // reader better than an empty archive does.
        guard jsonl.split(separator: "\n", omittingEmptySubsequences: true).count > 1 else {
            return nil
        }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(fileName)
        do {
            try jsonl.write(to: url, atomically: true, encoding: .utf8)
            return url
        } catch {
            return nil
        }
    }

    /// Whether the ring holds anything, for gating the share and delete
    /// buttons. Stops at the first row rather than serialising the archive to
    /// count it.
    static func hasCapturedEvents() -> Bool {
        (try? AppStore.get().hasProtocolEvents()) ?? false
    }

    /// Removes the exported copy. The ring itself is cleared separately, in
    /// the same block that clears the metrics tables -- both have to go, or the
    /// next share rebuilds the file that was just deleted.
    static func deleteExportedJSONL() {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(fileName)
        try? FileManager.default.removeItem(at: url)
    }
}

/// Identifiable wrapper so a freshly written set of export files can drive
/// `.sheet(item:)`. Plural because "Share diagnostics" shares the log file
/// alongside any `MetricKitCollector` JSON payloads in one sheet.
struct ShareableFile: Identifiable {
    let id = UUID()
    let urls: [URL]

    init(url: URL) { self.urls = [url] }
    init(urls: [URL]) { self.urls = urls }
}

/// Minimal UIActivityViewController bridge for sharing an on-demand file.
struct ActivityShareView: UIViewControllerRepresentable {
    let items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
