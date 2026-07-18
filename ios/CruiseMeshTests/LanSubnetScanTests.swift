import XCTest
@testable import CruiseMesh

final class LanSubnetScanTests: XCTestCase {
    func testSlash24StaysInsideSubnetAndExcludesSelfAndEdges() {
        let hosts = lanSubnetHosts(localAddress: "10.154.189.58", prefixLength: 24)
        XCTAssertEqual(hosts.count, 253)
        XCTAssertFalse(hosts.contains("10.154.189.58"))
        XCTAssertFalse(hosts.contains("10.154.189.0"))
        XCTAssertFalse(hosts.contains("10.154.189.255"))
        XCTAssertEqual(hosts.first, "10.154.189.1")
        XCTAssertEqual(hosts.last, "10.154.189.254")
    }

    func testSlash16CoversWholeSecondOctetRange() {
        let hosts = lanSubnetHosts(localAddress: "10.20.30.40", prefixLength: 16)
        XCTAssertEqual(hosts.count, 65_533)
        XCTAssertFalse(hosts.contains("10.20.30.40"))
        XCTAssertTrue(hosts.contains("10.20.0.1"))
        XCTAssertTrue(hosts.contains("10.20.99.99"))
        XCTAssertTrue(hosts.contains("10.20.255.254"))
    }

    func testBroaderNetworkClampsToSlash16AroundLocalAddress() {
        let hosts = lanSubnetHosts(localAddress: "10.20.30.40", prefixLength: 8)
        XCTAssertEqual(hosts.count, 65_533)
        XCTAssertFalse(hosts.contains("10.19.255.254"))
        XCTAssertFalse(hosts.contains("10.21.0.1"))
        XCTAssertTrue(hosts.contains("10.20.0.1"))
        XCTAssertTrue(hosts.contains("10.20.255.254"))
    }

    func testNarrowNetworkUsesItsActualBreadth() {
        XCTAssertEqual(
            lanSubnetHosts(localAddress: "192.168.1.5", prefixLength: 30),
            ["192.168.1.6"]
        )
    }

    func testPrefixAndNetmaskPolicies() {
        XCTAssertEqual(effectiveLanScanPrefixLength(8), 16)
        XCTAssertEqual(effectiveLanScanPrefixLength(22), 22)
        XCTAssertEqual(effectiveLanScanPrefixLength(32), 30)
        XCTAssertEqual(ipv4PrefixLength(netmask: 0xffff_0000), 16)
        XCTAssertEqual(ipv4PrefixLength(netmask: 0xffff_ff00), 24)
        XCTAssertNil(ipv4PrefixLength(netmask: 0xff00_ff00))
    }

    func testAutomaticScanLonelinessGateMatchesAndroid() {
        XCTAssertTrue(shouldRunAutomaticLanScan(activeConnections: 0, outboundAttempts: 0, scanRemaining: 0))
        XCTAssertFalse(shouldRunAutomaticLanScan(activeConnections: 1, outboundAttempts: 0, scanRemaining: 0))
        XCTAssertFalse(shouldRunAutomaticLanScan(activeConnections: 0, outboundAttempts: 1, scanRemaining: 0))
        XCTAssertFalse(shouldRunAutomaticLanScan(activeConnections: 0, outboundAttempts: 0, scanRemaining: 1))
    }

    func testBonjourPeerTokenRequiresVersionAndInstanceTxtRecords() {
        XCTAssertEqual(lanBonjourPeerToken(["v": "1", "i": "0011"]), "0011")
        XCTAssertNil(lanBonjourPeerToken(["v": "2", "i": "0011"]))
        XCTAssertNil(lanBonjourPeerToken(["v": "1"]))
        XCTAssertNil(lanBonjourPeerToken(["v": "1", "i": ""]))
    }
}
