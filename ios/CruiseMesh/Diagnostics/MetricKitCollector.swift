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
