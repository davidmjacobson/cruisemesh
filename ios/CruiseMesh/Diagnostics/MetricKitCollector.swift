import Foundation
import MetricKit
import os.log

/// Battery audit follow-up (2026-07-21): a minimal MetricKit subscriber.
/// `start()` is called once at app launch, from
/// `AppDelegate.application(_:didFinishLaunchingWithOptions:)`
/// (`CruiseMeshApp.swift`) rather than a SwiftUI `onAppear` -- that method
/// runs unconditionally on every launch, including a headless background BLE
/// relaunch that never shows any UI, and MetricKit wants subscribers added
/// as early as possible in every launch. The OS then delivers a daily
/// `MXMetricPayload` roughly once every 24 hours (plus sometimes one shortly
/// after launch covering the prior period) via `didReceive`.
///
/// Each payload is reduced to a small JSON summary -- `cpuMetrics`,
/// `applicationTimeMetrics`, and `networkTransferMetrics`, the battery/CPU/
/// network fields the audit called out -- and written into
/// `DiagnosticLogExport.metricKitDirectory()`, the same place the existing
/// "Share diagnostics" flow already looks for files to attach.
/// That means this rides the existing share flow with zero new UI: no new
/// screen, no new button, no new setting.
///
/// Metadata only, by construction, not by redaction: `MXMetricPayload` is
/// OS-aggregated per-app telemetry (cumulative durations and byte counts) --
/// it has no field capable of carrying message text, contact identities, or
/// any other app data, unlike `DiagnosticLogExport`'s log lines which are
/// metadata only because every call site was audited to avoid logging
/// content. `locationActivityMetrics` is deliberately not read here even
/// though it's nominally "battery data" -- it's about location-service
/// runtime, not battery/CPU/network, and this stays locationless on purpose.
///
/// The subscriber also takes `MXDiagnosticPayload`, which is the *only* way
/// this app can see why a previous launch died. `DiagnosticLogExport` reads
/// `OSLogStore(scope: .currentProcessIdentifier)` -- iOS offers no other
/// scope to a sandboxed app -- so a crash takes its own final log entries
/// with it, and the archive shows nothing but an unexplained gap followed by
/// a fresh "Mesh started". MetricKit closes that hole from the other side:
/// after a crash, hang, or watchdog kill, the *next* launch is handed a
/// payload describing the previous one, including a call-stack tree that
/// symbolicates against the dSYMs already uploaded to App Store Connect.
final class MetricKitCollector: NSObject, MXMetricManagerSubscriber {
    static let shared = MetricKitCollector()

    private static let log = Logger(subsystem: "com.cruisemesh", category: "MetricKitCollector")
    private static let iso8601 = ISO8601DateFormatter()

    private override init() {
        super.init()
    }

    /// Registers with `MXMetricManager`. Idempotent-ish in practice (the app
    /// only calls this once, at launch); safe to call more than once since
    /// `MXMetricManager.add` de-duplicates identical subscribers.
    func start() {
        MXMetricManager.shared.add(self)
    }

    func didReceive(_ payloads: [MXMetricPayload]) {
        for payload in payloads {
            write(payload)
        }
    }

    /// Crash/hang/exception reports for *previous* launches.
    ///
    /// The full `jsonRepresentation()` is written verbatim rather than
    /// summarized the way `MXMetricPayload` is: the call-stack tree is the
    /// entire point, it is deeply nested, and any field we dropped would be
    /// the one needed to read the next crash. It stays metadata by
    /// construction -- frames are binary UUIDs and offsets, and the
    /// surrounding fields are OS-authored (`terminationReason`, signal and
    /// exception numbers, device type, OS and app build version). No field
    /// carries message text or contact identities.
    ///
    /// A one-line summary also goes to the unified log so the human-readable
    /// archive says *that* the previous launch crashed and how, at the point
    /// in the timeline where it happened. Without it the JSON is easy to miss
    /// and the text log still reads as an unexplained restart.
    func didReceive(_ payloads: [MXDiagnosticPayload]) {
        for payload in payloads {
            summarizeToLog(payload)
            writeDiagnostic(payload)
        }
    }

    private func writeDiagnostic(_ payload: MXDiagnosticPayload) {
        guard let dir = DiagnosticLogExport.metricKitDirectory() else {
            Self.log.warning("Could not create MetricKit export directory")
            return
        }
        let stamp = Self.iso8601.string(from: payload.timeStampEnd)
            .replacingOccurrences(of: ":", with: "-")
        let url = dir.appendingPathComponent("diagnostic-\(stamp).json")
        do {
            try payload.jsonRepresentation().write(to: url, options: .atomic)
        } catch {
            Self.log.warning("Could not write MetricKit diagnostic: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func summarizeToLog(_ payload: MXDiagnosticPayload) {
        for crash in payload.crashDiagnostics ?? [] {
            // Every component is an OS-supplied number or short string; mark
            // them public so they survive `<private>` redaction in the archive.
            let reason = crash.terminationReason ?? "unknown"
            let signal = crash.signal?.stringValue ?? "-"
            let type = crash.exceptionType?.stringValue ?? "-"
            let code = crash.exceptionCode?.stringValue ?? "-"
            Self.log.error(
                """
                Previous launch CRASHED: \(reason, privacy: .public) \
                signal=\(signal, privacy: .public) \
                exceptionType=\(type, privacy: .public) \
                exceptionCode=\(code, privacy: .public) \
                build=\(crash.metaData.applicationBuildVersion, privacy: .public) \
                os=\(crash.metaData.osVersion, privacy: .public)
                """
            )
        }
        for hang in payload.hangDiagnostics ?? [] {
            let seconds = hang.hangDuration.converted(to: .seconds).value
            Self.log.error(
                """
                Previous launch HUNG for \(seconds, privacy: .public)s \
                (build \(hang.metaData.applicationBuildVersion, privacy: .public))
                """
            )
        }
        for cpu in payload.cpuExceptionDiagnostics ?? [] {
            let seconds = cpu.totalCPUTime.converted(to: .seconds).value
            Self.log.error(
                """
                Previous launch hit a CPU exception: \(seconds, privacy: .public)s CPU \
                (build \(cpu.metaData.applicationBuildVersion, privacy: .public))
                """
            )
        }
        for disk in payload.diskWriteExceptionDiagnostics ?? [] {
            let bytes = disk.totalWritesCaused.converted(to: .bytes).value
            Self.log.error(
                """
                Previous launch hit a disk-write exception: \(bytes, privacy: .public) bytes \
                (build \(disk.metaData.applicationBuildVersion, privacy: .public))
                """
            )
        }
    }

    private func write(_ payload: MXMetricPayload) {
        guard let dir = DiagnosticLogExport.metricKitDirectory() else {
            Self.log.warning("Could not create MetricKit export directory")
            return
        }
        let dict = summarize(payload)
        guard JSONSerialization.isValidJSONObject(dict),
              let data = try? JSONSerialization.data(withJSONObject: dict, options: [.prettyPrinted, .sortedKeys])
        else { return }
        let stamp = Self.iso8601.string(from: payload.timeStampEnd)
            .replacingOccurrences(of: ":", with: "-")
        let url = dir.appendingPathComponent("metrickit-\(stamp).json")
        do {
            try data.write(to: url, options: .atomic)
        } catch {
            Self.log.warning("Could not write MetricKit payload: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func summarize(_ payload: MXMetricPayload) -> [String: Any] {
        var dict: [String: Any] = [
            "periodStart": Self.iso8601.string(from: payload.timeStampBegin),
            "periodEnd": Self.iso8601.string(from: payload.timeStampEnd),
        ]
        if let cpu = payload.cpuMetrics {
            dict["cpuTimeSeconds"] = cpu.cumulativeCPUTime.converted(to: .seconds).value
        }
        if let appTime = payload.applicationTimeMetrics {
            dict["foregroundSeconds"] = appTime.cumulativeForegroundTime.converted(to: .seconds).value
            dict["backgroundSeconds"] = appTime.cumulativeBackgroundTime.converted(to: .seconds).value
        }
        if let network = payload.networkTransferMetrics {
            dict["wifiUploadBytes"] = network.cumulativeWifiUpload.converted(to: .bytes).value
            dict["wifiDownloadBytes"] = network.cumulativeWifiDownload.converted(to: .bytes).value
            dict["cellularUploadBytes"] = network.cumulativeCellularUpload.converted(to: .bytes).value
            dict["cellularDownloadBytes"] = network.cumulativeCellularDownload.converted(to: .bytes).value
        }
        return dict
    }
}
