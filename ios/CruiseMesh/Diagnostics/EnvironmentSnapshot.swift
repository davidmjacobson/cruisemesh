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
    /// Guards the two cached strings below.
    ///
    /// Not optional, and not a "diagnostics code doesn't need locks" tradeoff:
    /// these are written from an `NWPathMonitor` callback on a utility queue
    /// and read from whichever thread flushes the archive. A `String` holding
    /// more than 15 UTF-8 bytes -- which every value here does -- is a
    /// refcounted heap buffer, so an unsynchronised read can load a pointer
    /// the writer then releases, and retain freed memory. That is a crash, not
    /// a stale line, and the two race windows are *correlated*: backgrounding
    /// is both when iOS re-evaluates the network path and when
    /// `archiveCurrentSession()` runs. Diagnostics code crashing the app it is
    /// supposed to explain is the worst possible failure here.
    private static let lock = NSLock()

    /// Last network path seen by `MeshController`'s monitor. Set from its
    /// existing `pathUpdateHandler` rather than by starting a second
    /// `NWPathMonitor` here, which would duplicate a system resource just to
    /// print one line.
    nonisolated(unsafe) private static var networkSummary: String?

    static func record(path: NWPath) {
        let summary = describe(path)
        lock.lock()
        networkSummary = summary
        lock.unlock()
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
        let status: String
        switch UIApplication.shared.backgroundRefreshStatus {
        case .available: status = "available"
        case .denied: status = "DENIED"
        case .restricted: status = "restricted"
        @unknown default: status = "unknown"
        }
        lock.lock()
        backgroundRefresh = status
        lock.unlock()
    }

    /// One line for the launch banner. Deliberately terse and greppable.
    static func line() -> String {
        // Copy both cached strings under one lock, then format outside it.
        lock.lock()
        let refresh = backgroundRefresh
        let network = networkSummary ?? "unknown"
        lock.unlock()

        var parts: [String] = []
        parts.append("backgroundRefresh=\(refresh)")
        parts.append("lowPower=\(ProcessInfo.processInfo.isLowPowerModeEnabled)")
        parts.append("thermal=\(thermal())")
        parts.append("bluetooth=\(bluetoothAuthorization())")
        parts.append("network=\(network)")
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
