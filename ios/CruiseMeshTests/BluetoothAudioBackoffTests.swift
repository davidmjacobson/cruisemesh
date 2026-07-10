import XCTest
@testable import CruiseMesh

final class BluetoothAudioBackoffTests: XCTestCase {
    func testFirstDisconnectedSnapshotKeepsMeshActive() {
        let backoff = BluetoothAudioBackoff()
        XCTAssertEqual(backoff.update(bluetoothAudioActive: false), .active)
    }

    func testFirstConnectedSnapshotPausesMeshForAudio() {
        let backoff = BluetoothAudioBackoff()
        XCTAssertEqual(backoff.update(bluetoothAudioActive: true), .pausedForBluetoothAudio)
    }

    func testRepeatingSameStateIsNoOp() {
        let backoff = BluetoothAudioBackoff()
        _ = backoff.update(bluetoothAudioActive: true)
        XCTAssertNil(backoff.update(bluetoothAudioActive: true))
    }

    func testDisconnectingAfterPauseResumesMesh() {
        let backoff = BluetoothAudioBackoff()
        _ = backoff.update(bluetoothAudioActive: true)
        XCTAssertEqual(backoff.update(bluetoothAudioActive: false), .active)
    }

    func testResetAllowsSameStateToEmitAgain() {
        let backoff = BluetoothAudioBackoff()
        XCTAssertEqual(backoff.update(bluetoothAudioActive: false), .active)
        XCTAssertNil(backoff.update(bluetoothAudioActive: false))
        backoff.reset()
        XCTAssertEqual(backoff.update(bluetoothAudioActive: false), .active)
    }
}
