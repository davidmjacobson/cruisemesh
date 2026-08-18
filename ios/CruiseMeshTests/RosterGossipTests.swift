import Foundation
import XCTest
@testable import CruiseMesh

/// DL-3's receive rule, pinned.
///
/// The case that matters is the second one: a document that is perfectly valid,
/// correctly signed, and about somebody other than the person who sent it. The
/// signature chain has nothing to say about that — which is why the rule exists
/// and why it is worth a test that will fail loudly if the line is ever
/// "simplified" away while this shell still needs it.
///
/// The Swift twin of Android's `RosterGossipTest`. Both go when the shells move
/// their per-kind delivery onto `core_deliver_inbound`, whose own
/// `KIND_ROSTER_GOSSIP` arm makes the same check.
final class RosterGossipTests: XCTestCase {
    private func id(_ byte: UInt8) -> Data { Data(repeating: byte, count: 16) }

    func testAPersonsOwnDeviceListIsAccepted() {
        XCTAssertTrue(rosterGossipDescribesSender(rosterPersonId: id(1), senderUserId: id(1)))
    }

    func testAGenuineDocumentAboutSomebodyElseIsRefused() {
        // Not forged. Just replayed by a contact who also holds a copy -- and a
        // stale copy still vouches for a device its person has since buried.
        XCTAssertFalse(rosterGossipDescribesSender(rosterPersonId: id(2), senderUserId: id(1)))
    }

    func testAnEmptyPersonIdIsNeverTakenAsAMatch() {
        XCTAssertFalse(rosterGossipDescribesSender(rosterPersonId: Data(), senderUserId: Data()))
        XCTAssertFalse(rosterGossipDescribesSender(rosterPersonId: Data(), senderUserId: id(1)))
    }

    /// The kind number is a wire constant shared with Android and the core. A
    /// shell that drifts from it silently stops delivering DL-3 entirely.
    func testTheGossipKindMatchesTheWireConstant() {
        XCTAssertEqual(ProtocolKind.rosterGossip, 21)
    }
}
