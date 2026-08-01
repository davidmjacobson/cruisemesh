import XCTest
@testable import CruiseMesh

/// Edge detection only — the mesh no longer changes roles for Bluetooth audio,
/// so these assert transitions, not policy. Mirrors `A2dpAudioBackoffTest`.
final class BluetoothAudioBackoffTests: XCTestCase {
    func testFirstDisconnectedSnapshotReportsClear() {
        let backoff = BluetoothAudioBackoff()
        XCTAssertEqual(backoff.update(bluetoothAudioActive: false), .audioClear)
    }

    func testFirstConnectedSnapshotReportsConnected() {
        let backoff = BluetoothAudioBackoff()
        XCTAssertEqual(backoff.update(bluetoothAudioActive: true), .audioConnected)
    }

    func testRepeatingSameStateIsNoOp() {
        let backoff = BluetoothAudioBackoff()
        _ = backoff.update(bluetoothAudioActive: true)
        XCTAssertNil(backoff.update(bluetoothAudioActive: true))
    }

    func testDisconnectingAfterConnectedReportsClear() {
        let backoff = BluetoothAudioBackoff()
        _ = backoff.update(bluetoothAudioActive: true)
        XCTAssertEqual(backoff.update(bluetoothAudioActive: false), .audioClear)
    }

    func testResetAllowsSameStateToEmitAgain() {
        let backoff = BluetoothAudioBackoff()
        XCTAssertEqual(backoff.update(bluetoothAudioActive: false), .audioClear)
        XCTAssertNil(backoff.update(bluetoothAudioActive: false))
        backoff.reset()
        XCTAssertEqual(backoff.update(bluetoothAudioActive: false), .audioClear)
    }
}
