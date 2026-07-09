import CoreBluetooth
import Foundation

/// BLE GATT constants — must match Android `MeshConstants` byte-for-byte.
enum MeshConstants {
    static let serviceUUID = CBUUID(string: "6d657368-6372-7569-7365-6d657368a001")
    static let inboundCharacteristicUUID = CBUUID(string: "6d657368-6372-7569-7365-6d657368a002")
    static let outboundCharacteristicUUID = CBUUID(string: "6d657368-6372-7569-7365-6d657368a003")
    static let clientConfigDescriptorUUID = CBUUID(string: "00002902-0000-1000-8000-00805f9b34fb")

    /// Random per-process token for self-connection guard (Android parity).
    static let localInstanceId: Data = {
        var bytes = [UInt8](repeating: 0, count: 8)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        return Data(bytes)
    }()
}
