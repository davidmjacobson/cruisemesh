import XCTest
@testable import CruiseMesh

final class TickStatusTests: XCTestCase {
    func testDerivation() {
        XCTAssertEqual(tickStatusFor(lamport: 1, deliveredThrough: 0, readThrough: 0), .sent)
        XCTAssertEqual(tickStatusFor(lamport: 1, deliveredThrough: 1, readThrough: 0), .delivered)
        XCTAssertEqual(tickStatusFor(lamport: 1, deliveredThrough: 1, readThrough: 1), .read)
        // Read can outrun delivered under DTN.
        XCTAssertEqual(tickStatusFor(lamport: 2, deliveredThrough: 1, readThrough: 2), .read)
    }
}
