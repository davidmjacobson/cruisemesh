import XCTest
@testable import CruiseMesh

final class FriendImportFailureTextTests: XCTestCase {
    func testAFutureLinkSchemeAsksTheUserToUpdateTheApp() {
        XCTAssertEqual(
            friendImportFailureText(CoreError.UnsupportedLink, text: "CMFRIEND5:abc"),
            "This link needs a newer version of CruiseMesh. Update the app, then try again."
        )
        XCTAssertEqual(
            friendImportFailureText(CoreError.UnsupportedLink, text: "CMLINK1:abc"),
            "This link needs a newer version of CruiseMesh. Update the app, then try again."
        )
    }

    func testATruncatedKnownCardIsNotTreatedAsUnknownJunk() {
        XCTAssertEqual(
            friendImportFailureText(
                CoreError.InvalidFriendCard("truncated"),
                text: "CMFRIEND3:abc"
            ),
            "That looks like a friend card but part of it is missing. Copy the whole message and try again."
        )
    }

    func testUnrelatedPasteStaysAGenericNotACardMessage() {
        XCTAssertEqual(
            friendImportFailureText(NSError(domain: "test", code: 1), text: "hello"),
            "Not a CruiseMesh friend card"
        )
    }
}
