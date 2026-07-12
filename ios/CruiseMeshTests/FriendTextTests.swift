import XCTest
@testable import CruiseMesh

final class FriendTextTests: XCTestCase {
    func testExtractsTokenFromProse() {
        XCTAssertEqual(
            extractFriendToken("Add me:\nCMFRIEND1:abc123"),
            "CMFRIEND1:abc123"
        )
    }

    func testExtractsFirstToken() {
        XCTAssertEqual(
            extractFriendToken("CMFRIEND1:first CMFRIEND1:second"),
            "CMFRIEND1:first"
        )
    }

    func testReturnsTrimmedInputWhenNoToken() {
        XCTAssertEqual(
            extractFriendToken("  {\"name\":\"A\"} \n"),
            "{\"name\":\"A\"}"
        )
    }
}
