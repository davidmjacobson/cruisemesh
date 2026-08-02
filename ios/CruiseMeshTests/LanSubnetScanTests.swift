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

    func testAutomaticPrefixClampsToSlash20WhileManualStaysAtSlash16() {
        // A huge flat network (e.g. a cruise-ship /8 or /12) must clamp the
        // *automatic* sweep to /20 (~4,094 hosts), not the manual ceiling.
        XCTAssertEqual(effectiveAutomaticLanScanPrefixLength(8), 20)
        XCTAssertEqual(effectiveAutomaticLanScanPrefixLength(16), 20)
        XCTAssertEqual(effectiveAutomaticLanScanPrefixLength(20), 20)
        // Narrower actual networks are respected, same as the manual clamp.
        XCTAssertEqual(effectiveAutomaticLanScanPrefixLength(22), 22)
        XCTAssertEqual(effectiveAutomaticLanScanPrefixLength(32), 30)
        // The manual "Search local subnet" clamp is unchanged: still /16.
        XCTAssertEqual(effectiveLanScanPrefixLength(8), 16)
    }

    func testAutomaticSlash20HostCountIsFourThousandNinetyThree() {
        let hosts = lanSubnetHosts(
            localAddress: "10.20.30.40",
            prefixLength: effectiveAutomaticLanScanPrefixLength(8)
        )
        // 2^12 - 2 usable hosts in a /20, minus this phone.
        XCTAssertEqual(hosts.count, 4_093)
    }

    func testAutomaticScanLonelinessGateMatchesAndroid() {
        XCTAssertTrue(shouldRunAutomaticLanScan(
            activeConnections: 0, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 0
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            activeConnections: 1, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 0
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            activeConnections: 0, pendingOutboundAttempts: 1, scanRemaining: 0, unlinkedCapableContacts: 0
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            activeConnections: 0, pendingOutboundAttempts: 0, scanRemaining: 1, unlinkedCapableContacts: 0
        ))
    }

    func testUnlinkedCapableContactKeepsSweepGateOpenDespiteLiveLinks() {
        // One connected family member must not stop discovery of the rest.
        XCTAssertTrue(shouldRunAutomaticLanScan(
            activeConnections: 1, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 1
        ))
        XCTAssertTrue(shouldRunAutomaticLanScan(
            activeConnections: 3, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 2
        ))
        // But in-flight work still defers, links or not.
        XCTAssertFalse(shouldRunAutomaticLanScan(
            activeConnections: 1, pendingOutboundAttempts: 1, scanRemaining: 0, unlinkedCapableContacts: 1
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            activeConnections: 1, pendingOutboundAttempts: 0, scanRemaining: 7, unlinkedCapableContacts: 1
        ))
        // Everyone capable is linked: nothing left to sweep for.
        XCTAssertFalse(shouldRunAutomaticLanScan(
            activeConnections: 1, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 0
        ))
    }

    func testContactLastSeenOnALanLongAgoStopsMotivatingSweeps() {
        let day: Int64 = 24 * 60 * 60 * 1_000
        let now = 100 * day

        // Seen on a LAN within the window: still worth sweeping for.
        XCTAssertTrue(lanCapabilityMotivatesScan(lastSupportedAtMs: now, nowMs: now))
        XCTAssertTrue(lanCapabilityMotivatesScan(lastSupportedAtMs: now - 13 * day, nowMs: now))
        // A family member who went ashore two weeks ago does not keep every
        // remaining phone sweeping the subnet forever.
        XCTAssertFalse(lanCapabilityMotivatesScan(lastSupportedAtMs: now - 14 * day, nowMs: now))
        XCTAssertFalse(lanCapabilityMotivatesScan(lastSupportedAtMs: now - 400 * day, nowMs: now))
        // Never demonstrated LAN support at all (including capability
        // recorded before this timestamp existed).
        XCTAssertFalse(lanCapabilityMotivatesScan(lastSupportedAtMs: nil, nowMs: now))
        // A clock that moved backwards must not expire a fresh sighting.
        XCTAssertTrue(lanCapabilityMotivatesScan(lastSupportedAtMs: now + day, nowMs: now))
    }

    func testSweepGateClosesOnceEveryCapableContactHasGoneStale() {
        let now: Int64 = 50 * 24 * 60 * 60 * 1_000
        let stale = now - 30 * 24 * 60 * 60 * 1_000
        let capable: [Data: Int64] = [Data([1]): stale, Data([2]): stale]
        let motivating = capable.filter {
            lanCapabilityMotivatesScan(lastSupportedAtMs: $0.value, nowMs: now)
        }.count

        XCTAssertEqual(motivating, 0)
        // A live link plus no motivating contact means no sweep at all.
        XCTAssertFalse(shouldRunAutomaticLanScan(
            activeConnections: 1,
            pendingOutboundAttempts: 0,
            scanRemaining: 0,
            unlinkedCapableContacts: motivating
        ))
    }

    func testPerNetworkBookkeepingSetsStopGrowingAtTheirCap() {
        var keys = Set<String>()

        for index in 0..<4 {
            XCTAssertTrue(claimBoundedLanKey(&keys, "token-\(index)", limit: 4))
        }
        XCTAssertEqual(keys.count, 4)
        // At the cap a brand-new key is refused rather than remembered, so a
        // network full of fresh advertisements cannot grow this without end.
        XCTAssertFalse(claimBoundedLanKey(&keys, "token-4", limit: 4))
        XCTAssertEqual(keys.count, 4)
        // A key already claimed still reads as "not new work", cap or no cap.
        XCTAssertFalse(claimBoundedLanKey(&keys, "token-0", limit: 4))
        XCTAssertFalse(claimBoundedLanKey(&keys, "token-0", limit: 99))
    }

    func testSweepIsOnlyCreditedWithAFindWhileItIsStillTheRunningSweep() {
        let running = UUID()
        let replaced = UUID()

        XCTAssertTrue(lanSweepCreditApplies(
            sweepGeneration: running,
            runningSweepGeneration: running
        ))
        // A late handshake from a replaced sweep must not credit the new one.
        XCTAssertFalse(lanSweepCreditApplies(
            sweepGeneration: replaced,
            runningSweepGeneration: running
        ))
        // Completed or cancelled: nothing to credit.
        XCTAssertFalse(lanSweepCreditApplies(
            sweepGeneration: replaced,
            runningSweepGeneration: nil
        ))
        // A link no sweep dialed never credits one.
        XCTAssertFalse(lanSweepCreditApplies(
            sweepGeneration: nil,
            runningSweepGeneration: running
        ))
    }

    func testBonjourPeerTokenRequiresVersionAndInstanceTxtRecords() {
        XCTAssertEqual(lanBonjourPeerToken(["v": "1", "i": "0011"]), "0011")
        XCTAssertNil(lanBonjourPeerToken(["v": "2", "i": "0011"]))
        XCTAssertNil(lanBonjourPeerToken(["v": "1"]))
        XCTAssertNil(lanBonjourPeerToken(["v": "1", "i": ""]))
    }
}
