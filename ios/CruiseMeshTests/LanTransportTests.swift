import XCTest
@testable import CruiseMesh

final class LanTransportTests: XCTestCase {
    private func contact(userByte: UInt8, agreeByte: UInt8) -> Contact {
        Contact(
            userId: Data(repeating: userByte, count: 16),
            name: "Peer \(userByte)",
            signPk: Data(repeating: userByte &+ 1, count: 32),
            agreePk: Data(repeating: agreeByte, count: 32),
            relayUrl: nil,
            relayToken: nil
        )
    }

    func testDefaultPortAndBonjourType() {
        XCTAssertEqual(lanDefaultTcpPort(), 45_892)
        XCTAssertEqual(appleLanServiceType(), "_cruisemesh._tcp")
    }

    func testDiscoveryTokensElectExactlyOneInitiator() {
        XCTAssertTrue(shouldInitiateLanConnection(localToken: "0011", remoteToken: "aabb"))
        XCTAssertFalse(shouldInitiateLanConnection(localToken: "aabb", remoteToken: "0011"))
        XCTAssertFalse(shouldInitiateLanConnection(localToken: "aabb", remoteToken: "aabb"))
    }

    func testNoiseStaticKeyResolvesOnlyAcceptedContact() {
        let alice = contact(userByte: 1, agreeByte: 7)
        let bob = contact(userByte: 2, agreeByte: 8)

        XCTAssertEqual(
            trustedLanPeerUserId(
                contacts: [alice, bob],
                remoteStaticKey: bob.agreePk
            ),
            bob.userId
        )
        XCTAssertNil(
            trustedLanPeerUserId(
                contacts: [alice, bob],
                remoteStaticKey: Data(repeating: 9, count: 32)
            )
        )
    }
}
