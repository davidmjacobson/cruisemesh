import Foundation

/// Everything captured, bundled into one zip for "Share diagnostics".
///
/// The share used to hand the sheet a list of files -- the log, any MetricKit
/// crash payloads, the delivery-timings CSV. That is the documented way to
/// attach several, and it is correct on our side, but a receiving app is free
/// to take the first attachment and drop the rest, and several do (on Android,
/// saving to Files kept only the log). Nothing about that failure is visible:
/// the tester believes they sent diagnostics and support gets part of them.
///
/// A zip cannot be half-consumed. It also survives being forwarded through mail
/// and messaging apps intact, and "send me the one file" is a simpler thing to
/// ask a family member for than "make sure all four arrive".
enum DiagnosticsArchive {
    /// Zips `files` and returns the archive, or `nil` if it could not be
    /// written. The caller must share nothing on failure: share targets may
    /// silently discard all but the first loose diagnostics file.
    ///
    /// Uses `NSFileCoordinator`'s `.forUploading` on a staging directory --
    /// the platform's own zip, no third-party dependency. The staging
    /// directory is named for the archive, so unzipping yields one tidy
    /// folder rather than loose files in whatever the reader's Downloads is.
    static func write(files: [URL], name: String) -> URL? {
        guard !files.isEmpty else { return nil }
        let manager = FileManager.default
        let staging = manager.temporaryDirectory.appendingPathComponent(name, isDirectory: true)
        try? manager.removeItem(at: staging)
        guard (try? manager.createDirectory(at: staging, withIntermediateDirectories: true)) != nil else {
            return nil
        }
        defer { try? manager.removeItem(at: staging) }

        for file in files {
            let destination = staging.appendingPathComponent(file.lastPathComponent)
            // MetricKit payload names are timestamped and the other two are
            // fixed and distinct, so a collision means something upstream
            // changed; dropping a file silently is the one outcome to avoid.
            guard !manager.fileExists(atPath: destination.path) else { continue }
            try? manager.copyItem(at: file, to: destination)
        }
        guard ((try? manager.contentsOfDirectory(atPath: staging.path))?.isEmpty == false) else {
            return nil
        }

        let destination = archiveURL(name: name)
        try? manager.removeItem(at: destination)
        var written: URL?
        var coordinationError: NSError?
        NSFileCoordinator().coordinate(
            readingItemAt: staging,
            options: [.forUploading],
            error: &coordinationError
        ) { zipped in
            // A half-copied zip would share as a plausible attachment and fail
            // to open on the far side, so a failed copy leaves nothing behind.
            do {
                try manager.copyItem(at: zipped, to: destination)
                written = destination
            } catch {
                try? manager.removeItem(at: destination)
                written = nil
            }
        }
        return coordinationError == nil ? written : nil
    }

    /// Today's archive name, without the extension. Dated because the first
    /// thing anyone asks of a diagnostics file is when it was taken, and the
    /// file name is the only part that survives being forwarded through three
    /// apps.
    static func todaysName(date: Date = Date()) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        return "cruisemesh-diagnostics-\(formatter.string(from: date))"
    }

    /// Erases any archive a previous share left in the temporary directory.
    ///
    /// It is a full second copy of the log, the crash payloads and the
    /// metrics, so a "delete captured diagnostics" that left it behind would
    /// be untrue until the OS happened to reclaim tmp. Matches the same
    /// reasoning in `FieldMetricsExport.deleteExportedCSV`.
    static func deleteArchives() {
        let manager = FileManager.default
        let contents = (try? manager.contentsOfDirectory(
            at: manager.temporaryDirectory,
            includingPropertiesForKeys: nil
        )) ?? []
        for url in contents where url.lastPathComponent.hasPrefix("cruisemesh-diagnostics-")
            && url.pathExtension == "zip" {
            try? manager.removeItem(at: url)
        }
    }

    private static func archiveURL(name: String) -> URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("\(name).zip")
    }
}

enum DiagnosticsSharePlan: Equatable {
    case nothingCaptured
    case archive(URL)
    case archiveFailed

    /// Produces exactly one shareable archive or an explicit non-share state.
    /// There is intentionally no loose-file case in this type, making the
    /// field-data integrity rule enforceable instead of advisory.
    static func prepare(
        files: [URL],
        name: String,
        archiveWriter: ([URL], String) -> URL? = { files, name in
            DiagnosticsArchive.write(files: files, name: name)
        }
    ) -> DiagnosticsSharePlan {
        guard !files.isEmpty else { return .nothingCaptured }
        guard let archive = archiveWriter(files, name) else { return .archiveFailed }
        return .archive(archive)
    }
}
