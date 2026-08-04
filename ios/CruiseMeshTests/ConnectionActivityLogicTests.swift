import XCTest
@testable import CruiseMesh

final class ConnectionActivityLogicTests: XCTestCase {
    private func summary(
        _ transport: PeerConnectionTransport,
        connected: Int64? = nil,
        disconnected: Int64? = nil,
        seen: Int64? = nil,
        delivered: Int64? = nil,
        received: Int64? = nil
    ) -> PeerConnectionSummary {
        PeerConnectionSummary(
            userId: Data([1, 2, 3]),
            transport: transport,
            lastConnectedAtMs: connected,
            lastDisconnectedAtMs: disconnected,
            lastSeenAtMs: seen,
            lastDeliveredAtMs: delivered,
            lastReceivedAtMs: received
        )
    }

    func testNoEvidenceMeansNoStatusLine() {
        XCTAssertNil(ConnectionActivityLogic.latestPeerStatus([]))
        XCTAssertNil(ConnectionActivityLogic.latestPeerStatus([summary(.bluetooth)]))
    }

    /// The two message directions are separate fields and must map to separate
    /// evidence. Reporting an outbound delivery confirmation as an inbound
    /// arrival is the defect this whole change exists to remove.
    func testOutboundDeliveryAndInboundArrivalAreDistinct() {
        XCTAssertEqual(
            ConnectionActivityLogic.latestPeerStatus([summary(.shorePass, delivered: 500)])?.evidence,
            .messageDelivered
        )
        XCTAssertEqual(
            ConnectionActivityLogic.latestPeerStatus([summary(.shorePass, received: 500)])?.evidence,
            .messageReceived
        )
    }

    func testNewestTimestampOnARowWins() {
        let status = ConnectionActivityLogic.latestPeerStatus([
            summary(.bluetooth, connected: 100, disconnected: 200, seen: 900, delivered: 300, received: 400)
        ])
        XCTAssertEqual(status?.evidence, .presenceSeen)
        XCTAssertEqual(status?.atMs, 900)
    }

    /// Regression guard for the old row-first selection: it picked the row with
    /// the newest anything, then re-derived the evidence from a fixed field
    /// order, so a stale delivery confirmation could be reported as the latest
    /// news while a fresher arrival on another path was dropped.
    func testNewestMomentWinsAcrossPaths() {
        let status = ConnectionActivityLogic.latestPeerStatus([
            summary(.shorePass, seen: 2_000, delivered: 1_000),
            summary(.bluetooth, received: 5_000),
        ])
        XCTAssertEqual(status?.evidence, .messageReceived)
        XCTAssertEqual(status?.transport, .bluetooth)
        XCTAssertEqual(status?.atMs, 5_000)
    }

    func testReportedPathIsTheOneTheWinningMomentHappenedOn() {
        let status = ConnectionActivityLogic.latestPeerStatus([
            summary(.bluetooth, seen: 9_000),
            summary(.localWifi, received: 1_000),
        ])
        XCTAssertEqual(status?.transport, .bluetooth)
        XCTAssertEqual(status?.evidence, .presenceSeen)
    }

    /// A tie is broken towards the more informative evidence.
    func testInboundArrivalOutranksALinkEventAtTheSameInstant() {
        let status = ConnectionActivityLogic.latestPeerStatus([
            summary(.bluetooth, connected: 700, received: 700)
        ])
        XCTAssertEqual(status?.evidence, .messageReceived)
    }

    func testEveryEventKindMapsToItsOwnEvidence() {
        XCTAssertEqual(ConnectionActivityLogic.evidence(of: .connected), .connected)
        XCTAssertEqual(ConnectionActivityLogic.evidence(of: .disconnected), .disconnected)
        XCTAssertEqual(ConnectionActivityLogic.evidence(of: .presenceSeen), .presenceSeen)
        XCTAssertEqual(ConnectionActivityLogic.evidence(of: .messageDelivered), .messageDelivered)
        XCTAssertEqual(ConnectionActivityLogic.evidence(of: .messageReceived), .messageReceived)
    }

    /// The screen chooses between "… via Bluetooth" and the wordless variant by
    /// whether `transportLabel` returns a string. That must agree with core, or
    /// one surface names a path the other says was never seen. Every case is
    /// checked, so a transport added later cannot silently pick a default.
    func testAPathIsNamedExactlyWhenCoreSaysItWasObserved() {
        let all: [PeerConnectionTransport] = [.bluetooth, .localWifi, .shorePass, .carried]
        for transport in all {
            XCTAssertEqual(
                corePeerTransportIsObserved(transport: transport),
                ConnectionDetailsView.transportLabel(transport) != nil,
                "transportLabel disagrees with core for \(transport)"
            )
        }
    }

    func testACarriedMessageNamesNoPath() {
        XCTAssertNil(
            ConnectionDetailsView.transportLabel(.carried),
            "a message another device carried must not be labelled with a radio"
        )
    }
}
