import CoreBluetooth
import Foundation

/// BLE GATT constants — must match Android `MeshConstants` byte-for-byte.
///
/// Frozen protocol surface: this is the fixed 128-bit UUID space CruiseMesh
/// advertises/scans for over BLE, and Android discovers/advertises the exact
/// same values (see android .../mesh/MeshConstants.kt). Changing any of
/// these partitions the mesh -- old and new builds will not discover each
/// other. Do not edit without a coordinated fleet upgrade on both platforms.
enum MeshConstants {
    static let serviceUUID = CBUUID(string: "a5987315-cdcf-4e09-b036-ce10af3c05d3")
    static let inboundCharacteristicUUID = CBUUID(string: "a5987315-cdcf-4e09-b036-ce10af3c05d4")
    static let outboundCharacteristicUUID = CBUUID(string: "a5987315-cdcf-4e09-b036-ce10af3c05d5")
    static let clientConfigDescriptorUUID = CBUUID(string: "00002902-0000-1000-8000-00805f9b34fb")

    /// Random per-process token for self-connection guard (Android parity).
    static let localInstanceId: Data = {
        var bytes = [UInt8](repeating: 0, count: 8)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        return Data(bytes)
    }()

    /// FI3: stable CoreBluetooth state-restoration identifiers. Passed to
    /// `CBCentralManager`/`CBPeripheralManager` at creation
    /// (`BleTransport.init`) so that when iOS relaunches the app in the
    /// background after a BLE event (following jetsam or a manual kill --
    /// `UIBackgroundModes` already declares both `bluetooth-central` and
    /// `bluetooth-peripheral`), the system can correlate the new manager
    /// instances back to the ones that were scanning/advertising/connected
    /// before termination and deliver `willRestoreState`. Must stay stable
    /// across app versions/launches -- the system keys restoration off this
    /// exact string, not any in-memory identity.
    static let bleCentralRestoreIdentifier = "com.cruisemesh.ble.central"
    static let blePeripheralRestoreIdentifier = "com.cruisemesh.ble.peripheral"
}
