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
            peerLinks: 0, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 0, ownDeviceSearchLive: false
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            peerLinks: 1, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 0, ownDeviceSearchLive: false
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            peerLinks: 0, pendingOutboundAttempts: 1, scanRemaining: 0, unlinkedCapableContacts: 0, ownDeviceSearchLive: false
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            peerLinks: 0, pendingOutboundAttempts: 0, scanRemaining: 1, unlinkedCapableContacts: 0, ownDeviceSearchLive: false
        ))
    }

    func testUnlinkedCapableContactKeepsSweepGateOpenDespiteLiveLinks() {
        // One connected family member must not stop discovery of the rest.
        XCTAssertTrue(shouldRunAutomaticLanScan(
            peerLinks: 1, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 1, ownDeviceSearchLive: false
        ))
        XCTAssertTrue(shouldRunAutomaticLanScan(
            peerLinks: 3, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 2, ownDeviceSearchLive: false
        ))
        // But in-flight work still defers, links or not.
        XCTAssertFalse(shouldRunAutomaticLanScan(
            peerLinks: 1, pendingOutboundAttempts: 1, scanRemaining: 0, unlinkedCapableContacts: 1, ownDeviceSearchLive: false
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            peerLinks: 1, pendingOutboundAttempts: 0, scanRemaining: 7, unlinkedCapableContacts: 1, ownDeviceSearchLive: false
        ))
        // Everyone capable is linked: nothing left to sweep for.
        XCTAssertFalse(shouldRunAutomaticLanScan(
            peerLinks: 1, pendingOutboundAttempts: 0, scanRemaining: 0, unlinkedCapableContacts: 0, ownDeviceSearchLive: false
        ))
    }

    func testLinkToOneOfThisPersonsOwnDevicesNeverCountsAsCompany() {
        // The field case, on the approving phone: its only live LAN link was to
        // the device it had just removed. That link carries no contact's mail
        // and, having no route, sat outside the LAN heartbeat -- so a half-open
        // one used to read as "not lonely" and shut discovery off for the whole
        // Wi-Fi join. The transport passes peer links only, so the gate here
        // sees zero.
        XCTAssertTrue(shouldRunAutomaticLanScan(
            peerLinks: 0, pendingOutboundAttempts: 0, scanRemaining: 0,
            unlinkedCapableContacts: 0, ownDeviceSearchLive: false
        ))
        // A negative miscount slows discovery, never disables it.
        XCTAssertTrue(shouldRunAutomaticLanScan(
            peerLinks: -1, pendingOutboundAttempts: -3, scanRemaining: 0,
            unlinkedCapableContacts: 0, ownDeviceSearchLive: false
        ))
    }

    func testSiblingWithNoLinkKeepsTheSweepGateOpen() {
        // A device of this person's own shares their user id, so it has no
        // contact row and can never appear in unlinkedCapableContacts. Without
        // a motive of its own, Bonjour is the only channel between two phones
        // of one person -- and one stale Bonjour record is all the field
        // failure was. Whether that motive is live is OwnDeviceSearchWindow's
        // call, and it is bounded; see OwnDeviceSearchWindowTests.
        XCTAssertTrue(shouldRunAutomaticLanScan(
            peerLinks: 4, pendingOutboundAttempts: 0, scanRemaining: 0,
            unlinkedCapableContacts: 0, ownDeviceSearchLive: true
        ))
        XCTAssertFalse(shouldRunAutomaticLanScan(
            peerLinks: 4, pendingOutboundAttempts: 0, scanRemaining: 0,
            unlinkedCapableContacts: 0, ownDeviceSearchLive: false
        ))
        // In-flight work still defers.
        XCTAssertFalse(shouldRunAutomaticLanScan(
            peerLinks: 4, pendingOutboundAttempts: 2, scanRemaining: 0,
            unlinkedCapableContacts: 0, ownDeviceSearchLive: true
        ))
    }

    /// The field loop: a Bonjour-derived endpoint at an address that never
    /// answered (a link-local IPv6 one, which no other phone can dial), retried
    /// every retry period for as long as the phone stayed on the Wi-Fi.
    func testAddressThatNeverAnsweredStopsBeingRetried() {
        XCTAssertFalse(coreLanReconnectTargetIsExhausted(
            everAuthenticated: false, consecutiveFailures: 1
        ))
        XCTAssertTrue(coreLanReconnectTargetIsExhausted(
            everAuthenticated: false, consecutiveFailures: 6
        ))
        // An address a handshake proved is never retired by failure count:
        // ordinary contact LAN delivery has to survive a sleeping peer.
        XCTAssertFalse(coreLanReconnectTargetIsExhausted(
            everAuthenticated: true, consecutiveFailures: 60
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
            peerLinks: 1,
            pendingOutboundAttempts: 0,
            scanRemaining: 0,
            unlinkedCapableContacts: motivating,
            ownDeviceSearchLive: false
        ))
    }

    func testPerNetworkBookkeepingForgetsItsOldestKeyInsteadOfRefusingNewOnes() {
        var keys = BoundedLanKeySet(limit: 4)

        for index in 0..<4 {
            XCTAssertEqual(
                keys.claim("token-\(index)"),
                BoundedLanKeySet.Claim(isNew: true, evicted: nil)
            )
        }
        XCTAssertEqual(keys.count, 4)

        // A key already claimed is not new work, and costs nothing.
        XCTAssertEqual(keys.claim("token-0"), BoundedLanKeySet.Claim(isNew: false, evicted: nil))
        XCTAssertEqual(keys.count, 4)

        // At the cap a brand-new key is still accepted -- the OLDEST is
        // forgotten to make room. Refusing instead would let a flood of
        // made-up names lock a real family member out of the election
        // fallback for the rest of the network join.
        XCTAssertEqual(
            keys.claim("token-4"),
            BoundedLanKeySet.Claim(isNew: true, evicted: "token-0")
        )
        XCTAssertEqual(keys.count, 4)
        XCTAssertFalse(keys.contains("token-0"))
        XCTAssertTrue(keys.contains("token-4"))

        // A real peer arriving after a 100-name spray still reads as new.
        for index in 0..<100 { _ = keys.claim("spray-\(index)") }
        XCTAssertEqual(keys.count, 4)
        XCTAssertTrue(keys.claim("family-phone").isNew)
    }

    func testRemovedOrClearedBookkeepingKeyCanBeClaimedAgain() {
        var keys = BoundedLanKeySet(limit: 4)

        XCTAssertTrue(keys.claim("service-a").isNew)
        XCTAssertFalse(keys.claim("service-a").isNew)
        keys.remove("service-a")
        XCTAssertTrue(keys.claim("service-a").isNew)

        keys.removeAll()
        XCTAssertEqual(keys.count, 0)
        XCTAssertTrue(keys.claim("service-a").isNew)
    }

    func testSweepProbeThatCannotOpenALinkStillCountsAnAlreadyLinkedFriend() {
        // The probe collided with a service key an authenticated link already
        // holds: a healthy link keeps its key for its whole life, so this is
        // how every sweep after the one that linked the family sees them.
        XCTAssertTrue(lanSweepProbeFoundFriend(
            keyAlreadyAuthenticated: true, linkTableFull: false, authenticatedLinks: 1
        ))
        // The link table is full and a friend is on it: the healthiest
        // network there is, not an empty one.
        XCTAssertTrue(lanSweepProbeFoundFriend(
            keyAlreadyAuthenticated: false, linkTableFull: true, authenticatedLinks: 2
        ))
        // A full table of in-flight handshakes to unrelated services proves
        // nothing about friends being here.
        XCTAssertFalse(lanSweepProbeFoundFriend(
            keyAlreadyAuthenticated: false, linkTableFull: true, authenticatedLinks: 0
        ))
        // Colliding with an attempt that has not authenticated is not a find
        // either, however many other links exist.
        XCTAssertFalse(lanSweepProbeFoundFriend(
            keyAlreadyAuthenticated: false, linkTableFull: false, authenticatedLinks: 3
        ))
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
