import XCTest
@testable import CruiseMesh

/// A tapped `cruisemesh://lan#...` link used to queue a dial and start a
/// stopped mesh with no question asked. These pin the decision that replaced
/// that: never start the mesh, never dial off this Wi-Fi network, and only
/// offer to connect after showing the address. Matches Android's
/// `LanEndpointLinkTest`.
final class LanEndpointLinkDecisionTests: XCTestCase {
    private let peer = LanManualEndpoint(host: "192.168.86.23", port: 45_892)
    private let localEndpoint = "192.168.86.31:45892"

    func testLinkRoundTripsToTheSameEndpoint() {
        let link = lanEndpointLink(peer)
        let fragment = link.split(separator: "#", maxSplits: 1).last.map(String.init)
        XCTAssertEqual(parseLanEndpointLink(fragment), peer)
    }

    func testLinkWithMeshOffNeverOffersToConnect() {
        var compared = false
        let decision = decideLanEndpointLink(
            peer,
            meshRunning: false,
            localEndpoint: localEndpoint,
            sameNetwork: { _, _, _ in
                compared = true
                return true
            }
        )

        XCTAssertEqual(decision, .meshOff(peer))
        XCTAssertFalse(compared, "mesh-off links are not even evaluated")
    }

    func testLinkWithoutWifiAddressIsRefused() {
        XCTAssertEqual(
            decideLanEndpointLink(peer, meshRunning: true, localEndpoint: nil),
            .noWifi(peer)
        )
    }

    /// Uses the real core comparison, not the test seam.
    func testLinkToAnotherNetworkIsRefused() {
        let remote = LanManualEndpoint(host: "203.0.113.9", port: 45_892)
        XCTAssertEqual(
            decideLanEndpointLink(remote, meshRunning: true, localEndpoint: localEndpoint),
            .otherNetwork(remote)
        )
        let neighbourSubnet = LanManualEndpoint(host: "192.168.87.23", port: 45_892)
        XCTAssertEqual(
            decideLanEndpointLink(neighbourSubnet, meshRunning: true, localEndpoint: localEndpoint),
            .otherNetwork(neighbourSubnet)
        )
        // A hostname is never shown to be on this network.
        let named = LanManualEndpoint(host: "example.com", port: 45_892)
        XCTAssertEqual(
            decideLanEndpointLink(named, meshRunning: true, localEndpoint: localEndpoint),
            .otherNetwork(named)
        )
    }

    /// A ship's Wi-Fi is often one /16: two phones in different /24s of it are
    /// on the same network, so the interface's real prefix decides.
    func testLinkAcrossA24InsideTheLocal16AsksBeforeConnecting() {
        let shipPeer = LanManualEndpoint(host: "10.20.97.4", port: 45_892)
        XCTAssertEqual(
            decideLanEndpointLink(
                shipPeer,
                meshRunning: true,
                localEndpoint: "10.20.3.15:45892",
                localPrefixLength: 16
            ),
            .confirm(shipPeer)
        )
        // Without a reported prefix the fixed /24 still refuses it.
        XCTAssertEqual(
            decideLanEndpointLink(shipPeer, meshRunning: true, localEndpoint: "10.20.3.15:45892"),
            .otherNetwork(shipPeer)
        )
        let outside = LanManualEndpoint(host: "10.21.3.4", port: 45_892)
        XCTAssertEqual(
            decideLanEndpointLink(
                outside,
                meshRunning: true,
                localEndpoint: "10.20.3.15:45892",
                localPrefixLength: 16
            ),
            .otherNetwork(outside)
        )
    }

    func testLinkOnThisWifiAsksBeforeConnecting() {
        XCTAssertEqual(
            decideLanEndpointLink(peer, meshRunning: true, localEndpoint: localEndpoint),
            .confirm(peer)
        )
        let v6Peer = LanManualEndpoint(host: "2001:db8:1:2::23", port: 45_892)
        XCTAssertEqual(
            decideLanEndpointLink(v6Peer, meshRunning: true, localEndpoint: "[2001:db8:1:2::31]:45892"),
            .confirm(v6Peer)
        )
    }
}
