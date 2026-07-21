import XCTest
@testable import CruiseMesh

/// `carriedHopTtl` and `arrivalHopsTaken` are the pure logic behind the carry
/// (store-and-forward) hop-count fix: before this, the carry path stored
/// `hop_ttl` verbatim at every stage, so `arrivalHopsTaken` under-counted a
/// pure mule hand-off -- Message info showed "Arrived via another device ·
/// ~0 hops", a contradiction. Android twin: `MessageArrivalMetadataTest`.
final class CarryHopTtlTests: XCTestCase {
    func testCarriedHopTtlIsTheAuthoredValueMinusOne() {
        XCTAssertEqual(carriedHopTtl(7), 6)
        XCTAssertEqual(carriedHopTtl(1), 0)
    }

    func testCarriedHopTtlSaturatesAtZeroInsteadOfUnderflowing() {
        XCTAssertEqual(carriedHopTtl(0), 0)
    }

    func testHopCountIsDerivedFromTheOriginalBudgetAndSafelyClamped() {
        XCTAssertEqual(arrivalHopsTaken(receivedHopTtl: 7), 0)
        XCTAssertEqual(arrivalHopsTaken(receivedHopTtl: 5), 2)
        // Above the authored budget shouldn't happen (coreInboundGate
        // rejects it before this runs), but stays clamped rather than
        // underflowing if it ever does.
        XCTAssertEqual(arrivalHopsTaken(receivedHopTtl: 9), 0)
    }

    func testAOneMuleDeliveryReportsOneHopTakenNotZero() {
        // Regression for the field bug: a sender authors hopTtl 7, a single
        // mule carries it (storing carriedHopTtl(7) = 6) and hands it
        // straight to the recipient with that stored value -- the recipient
        // must see this as one hop, not the pre-fix "~0 hops" contradiction.
        let authoredHopTtl: UInt8 = 7
        let storedByMule = carriedHopTtl(authoredHopTtl)
        XCTAssertEqual(arrivalHopsTaken(receivedHopTtl: storedByMule), 1)
    }
}
