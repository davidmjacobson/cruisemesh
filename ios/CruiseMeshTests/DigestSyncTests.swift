import XCTest
@testable import CruiseMesh

final class DigestSyncTests: XCTestCase {
    private func userId(_ byte: UInt8) -> Data { Data(repeating: byte, count: 16) }

    func testChatIdMatchingHelloUserIdIsExpected() {
        let alice = userId(1)
        XCTAssertTrue(DigestSync.isExpectedChatId(digestChatId: alice, helloUserId: alice))
    }

    func testChatIdDifferingFromHelloUserIdIsRejected() {
        XCTAssertFalse(DigestSync.isExpectedChatId(digestChatId: userId(2), helloUserId: userId(1)))
    }

    func testDigestBeforeAnyHelloIsRejected() {
        XCTAssertFalse(DigestSync.isExpectedChatId(digestChatId: userId(1), helloUserId: nil))
    }

    func testMatchingIsByContentNotReference() {
        let digestChatId = userId(1)
        let helloUserId = userId(1)
        XCTAssertTrue(DigestSync.isExpectedChatId(digestChatId: digestChatId, helloUserId: helloUserId))
    }

    func testNoEntryForOurUserIdMeansSendEverything() {
        let own = userId(1)
        let entries = [DigestEntry(senderUserId: userId(2), throughLamport: 9)]
        XCTAssertEqual(DigestSync.throughLamportForSelf(entries: entries, ownUserId: own), 0)
    }

    func testEmptyDigestMeansSendEverything() {
        XCTAssertEqual(DigestSync.throughLamportForSelf(entries: [], ownUserId: userId(1)), 0)
    }

    func testEntryForOurUserIdReportsWhatPeerAlreadyHas() {
        let own = userId(1)
        let entries = [
            DigestEntry(senderUserId: userId(2), throughLamport: 100),
            DigestEntry(senderUserId: own, throughLamport: 7),
        ]
        XCTAssertEqual(DigestSync.throughLamportForSelf(entries: entries, ownUserId: own), 7)
    }

    func testEntriesAboutOtherSendersAreIgnored() {
        let own = userId(1)
        let entries = [
            DigestEntry(senderUserId: userId(2), throughLamport: 1),
            DigestEntry(senderUserId: userId(3), throughLamport: 2),
            DigestEntry(senderUserId: userId(4), throughLamport: 3),
        ]
        XCTAssertEqual(DigestSync.throughLamportForSelf(entries: entries, ownUserId: own), 0)
    }

    func testPeerAuthoredEntryLookedUpForReceiptSync() {
        let peer = userId(2)
        let entries = [
            DigestEntry(senderUserId: userId(1), throughLamport: 7),
            DigestEntry(senderUserId: peer, throughLamport: 11),
        ]
        XCTAssertEqual(DigestSync.throughLamportForSender(entries: entries, senderUserId: peer), 11)
    }
}
