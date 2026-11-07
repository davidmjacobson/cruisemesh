import XCTest
@testable import CruiseMesh

/// `BleTransport` reports through `BleEventGate`. On a background Bluetooth
/// relaunch, CoreBluetooth's `willRestoreState` runs before `MeshController`
/// has started, so the restored links are reported before anyone listens.
/// These tests pin that those reports are held and replayed, not lost.
final class BleEventGateTests: XCTestCase {

    private final class Recorder {
        var events: [BleTransportEvent] = []

        var handlers: BleTransportHandlers {
            BleTransportHandlers(
                onFrame: { self.events.append(.frame(address: $0, frame: $1)) },
                onCentralConnected: { self.events.append(.centralConnected(address: $0)) },
                onCentralDisconnected: { self.events.append(.centralDisconnected(address: $0)) },
                onPeripheralSubscribed: { self.events.append(.peripheralSubscribed(address: $0)) },
                onPeripheralUnsubscribed: { self.events.append(.peripheralUnsubscribed(address: $0)) }
            )
        }
    }

    func testRestoredSubscriptionReportedBeforeStartReachesTheMeshOnStart() {
        let gate = BleEventGate()
        // What `peripheralManager(_:willRestoreState:)` reports for a central
        // that was still subscribed when the process died.
        gate.emit(.peripheralSubscribed(address: "central-a"))

        let recorder = Recorder()
        gate.open(recorder.handlers)

        XCTAssertEqual(recorder.events, [.peripheralSubscribed(address: "central-a")])
        XCTAssertTrue(gate.bufferedEvents.isEmpty)
    }

    func testReplayKeepsArrivalOrderAcrossKinds() {
        let gate = BleEventGate()
        let hello = Data([0x01, 0x02])
        let expected: [BleTransportEvent] = [
            .peripheralSubscribed(address: "a"),
            .centralConnected(address: "b"),
            .frame(address: "b", frame: hello),
            .centralDisconnected(address: "b"),
            .peripheralUnsubscribed(address: "a"),
        ]
        expected.forEach(gate.emit)

        let recorder = Recorder()
        gate.open(recorder.handlers)

        XCTAssertEqual(recorder.events, expected)
    }

    func testEventsAfterStartAreDeliveredImmediately() {
        let gate = BleEventGate()
        let recorder = Recorder()
        gate.open(recorder.handlers)

        gate.emit(.centralConnected(address: "a"))

        XCTAssertEqual(recorder.events, [.centralConnected(address: "a")])
        XCTAssertTrue(gate.bufferedEvents.isEmpty)
    }

    func testOnlyFramesAreCappedWhileWaiting() {
        let gate = BleEventGate()
        for index in 0..<(BleEventGate.maxBufferedFrames + 10) {
            gate.emit(.frame(address: "a", frame: Data([UInt8(truncatingIfNeeded: index)])))
        }
        gate.emit(.peripheralSubscribed(address: "a"))

        let recorder = Recorder()
        gate.open(recorder.handlers)

        let frames = recorder.events.filter {
            if case .frame = $0 { return true }
            return false
        }
        XCTAssertEqual(frames.count, BleEventGate.maxBufferedFrames)
        // The earliest frames are the ones kept, and the connection event that
        // came after the cap was hit still arrives.
        XCTAssertEqual(frames.first, .frame(address: "a", frame: Data([0])))
        XCTAssertEqual(recorder.events.last, .peripheralSubscribed(address: "a"))
    }

    func testStopDropsHeldEventsAndLateReportsUntilTheNextStart() {
        let gate = BleEventGate()
        let first = Recorder()
        gate.open(first.handlers)
        gate.close()

        // A report from the session that just ended must not leak into it or
        // into the next one.
        gate.emit(.centralConnected(address: "stale"))
        XCTAssertTrue(first.events.isEmpty)
        XCTAssertTrue(gate.bufferedEvents.isEmpty)

        let second = Recorder()
        gate.open(second.handlers)
        gate.emit(.centralConnected(address: "fresh"))

        XCTAssertTrue(first.events.isEmpty)
        XCTAssertEqual(second.events, [.centralConnected(address: "fresh")])
    }

    func testCloseBeforeAnyStartDiscardsWhatTheRestoreHeld() {
        let gate = BleEventGate()
        gate.emit(.peripheralSubscribed(address: "a"))
        gate.close()

        let recorder = Recorder()
        gate.open(recorder.handlers)

        XCTAssertTrue(recorder.events.isEmpty)
    }
}
