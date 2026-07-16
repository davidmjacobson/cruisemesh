import XCTest
@testable import CruiseMesh

final class MeshRouterStateTests: XCTestCase {
    private func userId(_ byte: UInt8) -> Data { Data(repeating: byte, count: 16) }

    func testAddressWithNoHelloHasNoKnownUserId() {
        let state = MeshRouterState()
        state.onConnected(address: "AA:BB", transport: .central)
        XCTAssertNil(state.userIdFor(address: "AA:BB"))
        XCTAssertNil(state.routeFor(userId: userId(1)))
    }

    func testHelloOnConnectedAddressMakesItRoutable() {
        let state = MeshRouterState()
        let alice = userId(1)
        state.onConnected(address: "AA:BB", transport: .central)
        state.onHello(address: "AA:BB", userId: alice)

        XCTAssertEqual(state.userIdFor(address: "AA:BB"), alice)
        let route = state.routeFor(userId: alice)
        XCTAssertEqual(route?.0, .central)
        XCTAssertEqual(route?.1, "AA:BB")
    }

    func testHelloForNeverConnectedAddressIsNoOp() {
        let state = MeshRouterState()
        XCTAssertFalse(state.onHello(address: "AA:BB", userId: userId(1)))
        XCTAssertNil(state.userIdFor(address: "AA:BB"))
    }

    func testDisconnectingForgetsAddress() {
        let state = MeshRouterState()
        let alice = userId(1)
        state.onConnected(address: "AA:BB", transport: .peripheral)
        state.onHello(address: "AA:BB", userId: alice)
        XCTAssertEqual(state.routeFor(userId: alice)?.1, "AA:BB")

        state.onDisconnected(address: "AA:BB")

        XCTAssertNil(state.routeFor(userId: alice))
        XCTAssertNil(state.userIdFor(address: "AA:BB"))
        XCTAssertNil(state.transportFor(address: "AA:BB"))
    }

    func testSamePeerViaBothRolesRoutableWhileEitherLinkUp() {
        let state = MeshRouterState()
        let alice = userId(1)
        state.onConnected(address: "CENTRAL-LINK", transport: .central)
        state.onHello(address: "CENTRAL-LINK", userId: alice)
        state.onConnected(address: "PERIPHERAL-LINK", transport: .peripheral)
        state.onHello(address: "PERIPHERAL-LINK", userId: alice)

        let route = state.routeFor(userId: alice)
        XCTAssertNotNil(route)
        XCTAssertTrue(
            (route?.0 == .central && route?.1 == "CENTRAL-LINK")
                || (route?.0 == .peripheral && route?.1 == "PERIPHERAL-LINK")
        )

        state.onDisconnected(address: "CENTRAL-LINK")
        let remaining = state.routeFor(userId: alice)
        XCTAssertEqual(remaining?.0, .peripheral)
        XCTAssertEqual(remaining?.1, "PERIPHERAL-LINK")
    }

    func testTwoPeersNeverConfused() {
        let state = MeshRouterState()
        let alice = userId(1)
        let bob = userId(2)
        state.onConnected(address: "AA:BB", transport: .central)
        state.onHello(address: "AA:BB", userId: alice)
        state.onConnected(address: "CC:DD", transport: .peripheral)
        state.onHello(address: "CC:DD", userId: bob)

        XCTAssertEqual(state.routeFor(userId: alice)?.1, "AA:BB")
        XCTAssertEqual(state.routeFor(userId: bob)?.1, "CC:DD")
    }

    func testLanRouteIsPreferredWhileBleRemainsAsFallback() {
        let state = MeshRouterState()
        let alice = userId(1)
        state.onConnected(address: "BLE", transport: .central)
        state.onHello(address: "BLE", userId: alice)
        state.onConnected(address: "LAN", transport: .lan)
        state.onHello(address: "LAN", userId: alice)

        XCTAssertEqual(state.routeFor(userId: alice)?.0, .lan)
        XCTAssertEqual(state.routeFor(userId: alice)?.1, "LAN")

        state.onDisconnected(address: "LAN")
        XCTAssertEqual(state.routeFor(userId: alice)?.0, .central)
    }

    func testAuthenticatedIdentityCannotBeReplacedByHello() {
        let state = MeshRouterState()
        let alice = userId(1)
        state.onConnected(address: "LAN", transport: .lan)

        XCTAssertTrue(state.onHello(address: "LAN", userId: alice))
        XCTAssertFalse(state.onHello(address: "LAN", userId: userId(9)))
        XCTAssertEqual(state.userIdFor(address: "LAN"), alice)
    }

    func testClearingBleRoutesLeavesLanConnected() {
        let state = MeshRouterState()
        state.onConnected(address: "BLE", transport: .central)
        state.onConnected(address: "LAN", transport: .lan)

        state.clear(transports: [.central, .peripheral])

        XCTAssertNil(state.transportFor(address: "BLE"))
        XCTAssertEqual(state.transportFor(address: "LAN"), .lan)
    }

    func testTransportForReflectsRoleBeforeHello() {
        let state = MeshRouterState()
        state.onConnected(address: "AA:BB", transport: .central)
        XCTAssertEqual(state.transportFor(address: "AA:BB"), .central)
        XCTAssertNil(state.transportFor(address: "NEVER-CONNECTED"))
    }

    func testConnectedRoutesListsEveryLiveLink() {
        let state = MeshRouterState()
        state.onConnected(address: "AA:BB", transport: .central)
        state.onHello(address: "AA:BB", userId: userId(1))
        state.onConnected(address: "CC:DD", transport: .peripheral)

        let routes = Set(state.connectedRoutes().map { "\($0.0.rawValue):\($0.1)" })
        XCTAssertEqual(
            routes,
            Set(["central:AA:BB", "peripheral:CC:DD"])
        )
    }

    func testConnectedRoutesDropsDisconnectedLink() {
        let state = MeshRouterState()
        state.onConnected(address: "AA:BB", transport: .central)
        state.onConnected(address: "CC:DD", transport: .peripheral)
        state.onDisconnected(address: "AA:BB")

        let routes = state.connectedRoutes()
        XCTAssertEqual(routes.count, 1)
        XCTAssertEqual(routes[0].0, .peripheral)
        XCTAssertEqual(routes[0].1, "CC:DD")
    }
}
