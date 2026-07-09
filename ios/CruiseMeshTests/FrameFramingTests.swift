import XCTest
@testable import CruiseMesh

final class FrameFramingTests: XCTestCase {
    func testFragmentAndReassemble() {
        let frame = Data((0..<500).map { UInt8($0 % 251) })
        let fragments = FrameFraming.fragment(frame: frame, mtuPayloadSize: 50)
        XCTAssertGreaterThan(fragments.count, 1)
        let reassembler = FrameReassembler()
        var result: Data?
        for f in fragments {
            result = reassembler.accept(f)
        }
        XCTAssertEqual(result, frame)
    }

    func testUserIdHexRoundTrip() throws {
        let data = Data([0x0A, 0xFF, 0x00, 0x1B])
        let hex = UserIdHex.encode(data)
        XCTAssertEqual(hex, "0aff001b")
        XCTAssertEqual(try UserIdHex.decode(hex), data)
    }

    func testMeshRouterStateHelloRouting() {
        let state = MeshRouterState()
        state.onConnected(address: "a1", transport: .central)
        XCTAssertNil(state.routeFor(userId: Data([1, 2, 3])))
        state.onHello(address: "a1", userId: Data([1, 2, 3]))
        let route = state.routeFor(userId: Data([1, 2, 3]))
        XCTAssertEqual(route?.1, "a1")
        state.onDisconnected(address: "a1")
        XCTAssertNil(state.routeFor(userId: Data([1, 2, 3])))
    }
}
