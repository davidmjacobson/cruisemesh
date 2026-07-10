import Foundation

/**
 Tracks whether the BLE mesh should stay up or pause itself for Bluetooth
 audio coexistence (Android `A2dpAudioBackoff` / HANDOFF.md blocking item #4).

 Policy: any active Bluetooth audio route counts as "Bluetooth audio in use",
 so `MeshController` pauses both BLE roles entirely until that route is gone.
 The class is framework-free so the transition logic is unit-testable.

 ## Why not Classic A2DP profile state?

 Android can query `BluetoothProfile.A2DP` connection state directly. iOS has
 **no public A2DP/HFP profile-connection API**. The practical equivalent is
 `AVAudioSession` current-route ports (`.bluetoothA2DP`, `.bluetoothHFP`,
 `.bluetoothLE`). That fires when a headset is the active audio route — the
 case that actually contends with CoreBluetooth for radio time — rather than
 when a paired accessory is merely bonded. This is intentional, not an
 omission: CoreBluetooth already shares scheduling with Classic BT under
 Apple's stack, and pausing only for an active BT-audio route matches the
 user-visible "earbuds in use" problem Android's A2DP check targets.
 */
final class BluetoothAudioBackoff {
    enum Mode: Equatable {
        case active
        case pausedForBluetoothAudio
    }

    private var mode: Mode?

    /**
     Returns the newly desired mode when `bluetoothAudioActive` changes it,
     or `nil` if the desired mode is unchanged.
     */
    func update(bluetoothAudioActive: Bool) -> Mode? {
        let desired: Mode = bluetoothAudioActive ? .pausedForBluetoothAudio : .active
        if mode == desired { return nil }
        mode = desired
        return desired
    }

    /** Forget last mode so the next `update` always emits (mesh restart). */
    func reset() {
        mode = nil
    }
}
