import Foundation
import CoreBluetooth
import Network
import UIKit

/// The device conditions that silently stop the mesh working.
///
/// Every one of these can leave a log that looks like a healthy app doing
/// nothing at all: Background App Refresh off means DTN and relay sync cannot
/// run once the app leaves the foreground; Low Power Mode and thermal
/// throttling squeeze background work and the radios; a constrained or
/// expensive path suppresses sync; a full-tunnel VPN takes the default route
/// out from under relay binding; a nearly full disk triggers jetsam and lets
/// the OS reclaim caches. None of it produces an error line, so without a
/// snapshot the reader is left comparing an uneventful log against a report
/// that "it just stopped working".
///
/// Read on demand and written into the per-launch banner, so this costs
/// nothing while the app runs. Metadata only: interface *types*, never an
/// SSID, and never an address.
enum EnvironmentSnapshot {
    /// Last network path seen by `MeshController`'s monitor. Set from its
    /// existing `pathUpdateHandler` rather than by starting a second
    /// `NWPathMonitor` here, which would duplicate a system resource just to
    /// print one line.
    ///
    /// `nonisolated(unsafe)` because the path handler runs on a utility queue
    /// and the banner reads from whichever thread is archiving; a torn read of
    /// a `String?` is not a risk worth a lock in diagnostics code.
    nonisolated(unsafe) static var networkSummary: String?

    static func record(path: NWPath) {
        networkSummary = describe(path)
    }

    /// Last Background App Refresh status, sampled on the main actor.
    ///
    /// `UIApplication.shared` is main-actor isolated and the banner is written
    /// from whichever thread flushes the archive -- often a background one --
    /// so this is cached at launch and refreshed when the OS says it changed,
    /// rather than reached for at print time.
    nonisolated(unsafe) private static var backgroundRefresh = "not-sampled"

    /// Called once from `AppDelegate`, alongside `MetricKitCollector.start()`.
    @MainActor
    static func start() {
        sampleBackgroundRefresh()
        NotificationCenter.default.addObserver(
            forName: UIApplication.backgroundRefreshStatusDidChangeNotification,
            object: nil,
            queue: .main
        ) { _ in
            MainActor.assumeIsolated { sampleBackgroundRefresh() }
        }
    }

    @MainActor
    private static func sampleBackgroundRefresh() {
        switch UIApplication.shared.backgroundRefreshStatus {
        case .available: backgroundRefresh = "available"
        case .denied: backgroundRefresh = "DENIED"
        case .restricted: backgroundRefresh = "restricted"
        @unknown default: backgroundRefresh = "unknown"
        }
    }

    /// One line for the launch banner. Deliberately terse and greppable.
    static func line() -> String {
        var parts: [String] = []
        parts.append("backgroundRefresh=\(backgroundRefresh)")
        parts.append("lowPower=\(ProcessInfo.processInfo.isLowPowerModeEnabled)")
        parts.append("thermal=\(thermal())")
        parts.append("bluetooth=\(bluetoothAuthorization())")
        parts.append("network=\(networkSummary ?? "unknown")")
        if let free = freeDiskBytes() {
            parts.append("freeDisk=\(free / 1_048_576)MB")
        }
        return "      environment: " + parts.joined(separator: " ")
    }

    private static func thermal() -> String {
        switch ProcessInfo.processInfo.thermalState {
        case .nominal: return "nominal"
        case .fair: return "fair"
        case .serious: return "serious"
        case .critical: return "critical"
        @unknown default: return "unknown"
        }
    }

    /// Bluetooth permission without instantiating a `CBCentralManager` --
    /// creating one purely to read authorization would trigger the system
    /// permission prompt as a side effect of writing a log line.
    private static func bluetoothAuthorization() -> String {
        switch CBManager.authorization {
        case .allowedAlways: return "allowed"
        case .denied: return "DENIED"
        case .restricted: return "restricted"
        case .notDetermined: return "notDetermined"
        @unknown default: return "unknown"
        }
    }

    /// Interface types and constraints, never an SSID or an address.
    ///
    /// A VPN shows up as an `.other` interface alongside the real one, which
    /// is the fingerprint of the always-on full-tunnel setup that has
    /// confounded relay-binding debugging before.
    private static func describe(_ path: NWPath) -> String {
        var flags: [String] = [path.status == .satisfied ? "satisfied" : "\(path.status)"]
        var interfaces: [String] = []
        if path.usesInterfaceType(.wifi) { interfaces.append("wifi") }
        if path.usesInterfaceType(.cellular) { interfaces.append("cellular") }
        if path.usesInterfaceType(.wiredEthernet) { interfaces.append("wired") }
        if path.usesInterfaceType(.other) { interfaces.append("other/vpn") }
        if path.usesInterfaceType(.loopback) { interfaces.append("loopback") }
        flags.append(interfaces.isEmpty ? "no-interface" : interfaces.joined(separator: "+"))
        if path.isExpensive { flags.append("expensive") }
        if path.isConstrained { flags.append("constrained") }
        return flags.joined(separator: ",")
    }

    private static func freeDiskBytes() -> Int64? {
        guard let url = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        ) else {
            return nil
        }
        let values = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
        return values?.volumeAvailableCapacityForImportantUsage
    }
}
