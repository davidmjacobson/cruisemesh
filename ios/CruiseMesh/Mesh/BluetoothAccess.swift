import CoreBluetooth
import Foundation
import SwiftUI
import UIKit

/// Observes Bluetooth authorization + radio power so the home screen can
/// show a hard-to-miss banner when the mesh cannot work as designed.
@MainActor
final class BluetoothAccess: NSObject, ObservableObject {
    static let shared = BluetoothAccess()

    @Published private(set) var authorization: CBManagerAuthorization = CBCentralManager.authorization
    @Published private(set) var radioState: CBManagerState = .unknown

    private var central: CBCentralManager!

    private override init() {
        super.init()
        central = CBCentralManager(
            delegate: self,
            queue: nil,
            options: [CBCentralManagerOptionShowPowerAlertKey: false]
        )
        radioState = central.state
        authorization = CBCentralManager.authorization
    }

    /// True when iOS will not let us use Bluetooth at all (denied / restricted).
    var isAuthorizationBlocked: Bool {
        switch authorization {
        case .denied, .restricted: return true
        default: return false
        }
    }

    var isRadioOff: Bool {
        radioState == .poweredOff
    }

    /// Mesh cannot send/receive nearby as designed.
    var isBlocking: Bool {
        isAuthorizationBlocked || isRadioOff
    }

    func openSystemSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }
}

extension BluetoothAccess: CBCentralManagerDelegate {
    nonisolated func centralManagerDidUpdateState(_ central: CBCentralManager) {
        let state = central.state
        let auth = CBCentralManager.authorization
        Task { @MainActor in
            self.radioState = state
            self.authorization = auth
        }
    }
}
