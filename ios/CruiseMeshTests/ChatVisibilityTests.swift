import XCTest
@testable import CruiseMesh

final class ChatVisibilityTests: XCTestCase {
    private func userId(_ byte: UInt8) -> Data { Data(repeating: byte, count: 16) }

    override func setUp() {
        super.setUp()
        ChatVisibility.reset()
    }

    override func tearDown() {
        ChatVisibility.reset()
        super.tearDown()
    }

    func testNothingVisibleInitially() {
        XCTAssertFalse(ChatVisibility.isVisible(userId(1)))
    }

    func testSetVisibleMakesExactlyThatChatVisible() {
        ChatVisibility.setVisible(userId(1))
        XCTAssertTrue(ChatVisibility.isVisible(userId(1)))
        XCTAssertFalse(ChatVisibility.isVisible(userId(2)))
    }

    func testMatchingIsByContent() {
        ChatVisibility.setVisible(userId(1))
        XCTAssertTrue(ChatVisibility.isVisible(userId(1)))
    }

    func testClearVisibleRemovesMatchingRegistration() {
        ChatVisibility.setVisible(userId(1))
        ChatVisibility.clearVisible(userId(1))
        XCTAssertFalse(ChatVisibility.isVisible(userId(1)))
    }

    func testClearVisibleForDifferentChatIsNoOp() {
        ChatVisibility.setVisible(userId(1))
        ChatVisibility.setVisible(userId(2))
        ChatVisibility.clearVisible(userId(1))
        XCTAssertTrue(ChatVisibility.isVisible(userId(2)))
    }

    func testSetVisibleReplacesPreviousChat() {
        ChatVisibility.setVisible(userId(1))
        ChatVisibility.setVisible(userId(2))
        XCTAssertFalse(ChatVisibility.isVisible(userId(1)))
        XCTAssertTrue(ChatVisibility.isVisible(userId(2)))
    }

    func testClearVisibleWhenNothingVisibleIsNoOp() {
        ChatVisibility.clearVisible(userId(1))
        XCTAssertFalse(ChatVisibility.isVisible(userId(1)))
    }
}
