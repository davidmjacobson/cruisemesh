import Foundation

/**
 Edge detector for "is Bluetooth audio routed right now", so `MeshController`
 logs and surfaces the transition once rather than on every route notification.
 The class is framework-free so the transition logic is unit-testable.

 ## This no longer pauses anything

 It used to: any active Bluetooth audio route meant both BLE roles stopped
 until the route was gone, and this file's doc comment said that mirrored
 Android. It did, for 86 minutes. Android shipped that policy at 22:17 on
 2026-07-09 and deleted it at 23:43 the same evening, because messaging was
 dead on a phone whenever earbuds were connected — the relaxed low-power
 scan/advertise settings are the real coexistence mitigation. iOS kept the
 abandoned half. On a ship, earbuds are most of a sea day, and the person
 wearing them is precisely the peer the mesh needs to keep carrying traffic.

 iOS also has strictly *less* radio control than Android: CoreBluetooth exposes
 no scan-mode or connection-interval knob, so it took Android's stricter policy
 without ever having Android's actual mitigation. The mesh now stays up, and
 the route is informational only.

 ## Why the audio route, not Classic profile state

 Android can query `BluetoothProfile.A2DP` connection state directly. iOS has
 **no public A2DP/HFP profile-connection API**. The practical equivalent is
 `AVAudioSession` current-route ports (`.bluetoothA2DP`, `.bluetoothHFP`,
 `.bluetoothLE`), which fire when a headset is the active audio route rather
 than merely bonded. Android's signal is the broader one; both now feed a
 banner rather than a policy, so the difference is cosmetic.
 */
final class BluetoothAudioBackoff {
    enum Mode: Equatable {
        case audioClear
        case audioConnected
    }

    private var mode: Mode?

    /**
     Returns the new mode when `bluetoothAudioActive` changes it, or `nil` if
     it is unchanged.
     */
    func update(bluetoothAudioActive: Bool) -> Mode? {
        let desired: Mode = bluetoothAudioActive ? .audioConnected : .audioClear
        if mode == desired { return nil }
        mode = desired
        return desired
    }

    /** Forget last mode so the next `update` always emits (mesh restart). */
    func reset() {
        mode = nil
    }
}
