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
}

/// Identifiable wrapper so a freshly written export file can drive `.sheet(item:)`.
struct ShareableFile: Identifiable {
    let id = UUID()
    let url: URL
}

/// Minimal UIActivityViewController bridge for sharing an on-demand file.
struct ActivityShareView: UIViewControllerRepresentable {
    let items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
