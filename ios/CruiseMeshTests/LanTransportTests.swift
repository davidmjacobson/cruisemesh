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

    func testManualEndpointParserAndLinkRoundTrip() {
        XCTAssertEqual(
            parseLanManualEndpoint("10.12.3.4"),
            LanManualEndpoint(host: "10.12.3.4", port: 45_892)
        )
        XCTAssertEqual(
            parseLanManualEndpoint("[fe80::1]:5555"),
            LanManualEndpoint(host: "fe80::1", port: 5_555)
        )
        XCTAssertNil(parseLanManualEndpoint("host name"))

        let endpoint = LanManualEndpoint(host: "10.12.3.4", port: 45_892)
        let link = lanEndpointLink(endpoint)
        XCTAssertEqual(
            parseLanEndpointLink(URL(string: link)?.fragment),
            endpoint
        )
    }

    func testSubnetCandidatesAreBoundedToOneSlash24() {
        let hosts = subnet24Hosts(localAddress: "10.154.189.58")
        XCTAssertEqual(hosts.count, 253)
        XCTAssertFalse(hosts.contains("10.154.189.58"))
        XCTAssertTrue(hosts.contains("10.154.189.1"))
        XCTAssertTrue(hosts.contains("10.154.189.254"))
        XCTAssertFalse(hosts.contains("10.154.188.1"))
    }

    func testNetworkFingerprintMatchesAndroidEncoding() {
        XCTAssertEqual(
            lanNetworkId(ipv4Address: "10.154.189.58"),
            "NcJ68sf-sL-VO63PUTnngg=="
        )
        XCTAssertEqual(
            lanNetworkId(ipv4Address: "10.154.189.201"),
            "NcJ68sf-sL-VO63PUTnngg=="
        )
    }

    func testTransportSendPlanRacesSmallFramesButNotLargeOnes() {
        let routes: [(MeshRouterState.Transport, String)] = [
            (.lan, "LAN"),
            (.central, "BLE-1"),
            (.peripheral, "BLE-2"),
        ]
        XCTAssertEqual(
            transportSendPlan(routes: routes, frameSize: 512).map(\.1),
            ["LAN", "BLE-1"]
        )
        XCTAssertEqual(
            transportSendPlan(routes: routes, frameSize: 64 * 1_024).map(\.1),
            ["LAN"]
        )
    }

    // FI7: Local Network permission surfacing.

    func testKnownLocalNetworkPermissionErrorsAreRecognized() {
        XCTAssertTrue(isKnownLocalNetworkPermissionError(.dns(-65_570)))
        XCTAssertTrue(isKnownLocalNetworkPermissionError(.posix(.EPERM)))
        XCTAssertFalse(isKnownLocalNetworkPermissionError(.dns(-1)))
        XCTAssertFalse(isKnownLocalNetworkPermissionError(.posix(.ETIMEDOUT)))
        XCTAssertFalse(isKnownLocalNetworkPermissionError(.tls(-9_800)))
    }

    func testLocalNetworkPermissionWarningOnlyFiresAfterSustainedWaitOnReachableWifi() {
        // No Wi-Fi yet: never warn, no matter how long it's been waiting --
        // that's the ordinary "waiting for Wi-Fi" case, reported elsewhere.
        XCTAssertFalse(shouldWarnAboutLocalNetworkPermission(
            waitingSinceMs: 0, nowMs: 100_000, thresholdMs: 4_000, wifiReachable: false
        ))
        // Wi-Fi reachable but not waiting at all.
        XCTAssertFalse(shouldWarnAboutLocalNetworkPermission(
            waitingSinceMs: nil, nowMs: 100_000, thresholdMs: 4_000, wifiReachable: true
        ))
        // Wi-Fi reachable, waiting, but under threshold: not yet.
        XCTAssertFalse(shouldWarnAboutLocalNetworkPermission(
            waitingSinceMs: 10_000, nowMs: 12_000, thresholdMs: 4_000, wifiReachable: true
        ))
        // Wi-Fi reachable, waiting past threshold: looks like a denial.
        XCTAssertTrue(shouldWarnAboutLocalNetworkPermission(
            waitingSinceMs: 10_000, nowMs: 14_000, thresholdMs: 4_000, wifiReachable: true
        ))
        // Exactly at the threshold counts.
        XCTAssertTrue(shouldWarnAboutLocalNetworkPermission(
            waitingSinceMs: 10_000, nowMs: 14_000, thresholdMs: 4_000, wifiReachable: true
        ))
    }

    func testMessageInfoShowsLanArrivalAndDeliveryConfirmation() {
        let timestamp: Int64 = 1_700_000_000_000
        let message = StoredMessage(
            chatId: Data(repeating: 1, count: 16),
            senderUserId: Data(repeating: 2, count: 16),
            lamport: 4,
            timestamp: timestamp,
            kind: ProtocolKind.text,
            payload: Data("hello".utf8)
        )
        let direct = MessageArrival(transport: 3, hopsTaken: 0, receivedAt: timestamp)

        XCTAssertTrue(
            messageInfoRows(message: message, isOwn: false, tick: nil, arrival: direct).contains {
                if case .sentence(let text) = $0 { return text.contains("Arrived via local Wi-Fi · ~0 hops") }
                return false
            }
        )
        // T6: an own message's confirmation route is resolved from the delivery
        // receipt watermark (transport -> route) and passed in by the caller.
        XCTAssertTrue(
            messageInfoRows(
                message: message,
                isOwn: true,
                tick: .delivered,
                deliveredViaRoute: transportRouteText(4)
            ).contains {
                if case .sentence(let text) = $0 { return text.contains("Delivery confirmed via another device over local Wi-Fi") }
                return false
            }
        )
    }
}

final class LanHealthTrackerTests: XCTestCase {
    func testProbeTimeoutsEventuallyCloseLinkAndResponseResetsFailures() {
        let tracker = LanHealthTracker(timeoutMs: 10, maxConsecutiveTimeouts: 3)
        XCTAssertEqual(tracker.next(address: "LAN", nowMs: 0, nonce: 1), .send(1))
        XCTAssertEqual(tracker.next(address: "LAN", nowMs: 5, nonce: 2), .wait)
        XCTAssertEqual(tracker.next(address: "LAN", nowMs: 10, nonce: 2), .send(2))
        XCTAssertEqual(tracker.response(address: "LAN", nonce: 2, nowMs: 14), 4)
        XCTAssertEqual(tracker.next(address: "LAN", nowMs: 20, nonce: 3), .send(3))
        XCTAssertEqual(tracker.next(address: "LAN", nowMs: 30, nonce: 4), .send(4))
        XCTAssertEqual(tracker.next(address: "LAN", nowMs: 40, nonce: 5), .send(5))
        XCTAssertEqual(tracker.next(address: "LAN", nowMs: 50, nonce: 6), .close)
    }
}
