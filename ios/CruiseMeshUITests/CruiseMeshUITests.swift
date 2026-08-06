import Foundation
import XCTest

final class CruiseMeshUITests: XCTestCase {
    private var app: XCUIApplication!
    private var scenario = ""

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    override func tearDown() {
        if let app, (testRun?.failureCount ?? 0) > 0 {
            let screenshot = XCTAttachment(screenshot: app.screenshot())
            screenshot.name = "Failure-\(scenario)"
            screenshot.lifetime = .keepAlways
            add(screenshot)

            let hierarchy = XCTAttachment(
                data: Data(app.debugDescription.utf8),
                uniformTypeIdentifier: "public.plain-text"
            )
            hierarchy.name = "Accessibility-hierarchy-\(scenario)"
            hierarchy.lifetime = .keepAlways
            add(hierarchy)
        }
        app?.terminate()
        app = nil
        super.tearDown()
    }

    func testTermsGateRevealsOnboardingOnlyAfterAgreement() {
        launch(scenario: "terms")

        XCTAssertTrue(element("screen.terms").waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Before you start"].exists)

        let agree = app.buttons["I agree"]
        XCTAssertTrue(agree.exists)
        XCTAssertFalse(agree.isEnabled)

        app.switches["I have read and agree to the Terms of Use and Privacy Policy."].tap()
        XCTAssertTrue(agree.isEnabled)
        agree.tap()

        XCTAssertTrue(element("screen.onboarding").waitForExistence(timeout: 5))
        XCTAssertFalse(element("screen.chat-list").exists)
    }

    func testOnboardingCompletesIntoUsableHome() {
        launch(scenario: "onboarding")

        XCTAssertTrue(element("screen.onboarding").waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Messages that find a way through"].exists)

        for _ in 0..<4 {
            let next = app.buttons["Next"]
            XCTAssertTrue(next.waitForExistence(timeout: 3))
            next.tap()
        }

        let name = app.textFields["Your name"]
        XCTAssertTrue(name.waitForExistence(timeout: 3))
        name.tap()
        name.typeText("UI Tester")

        let start = app.buttons["Start using CruiseMesh"]
        XCTAssertTrue(start.isEnabled)
        start.tap()

        assertUsableHome()
    }

    func testHomeOpensAndDismissesFriendsAndSettings() {
        launch(scenario: "home-empty")
        assertUsableHome()

        app.buttons["Add a friend"].tap()
        XCTAssertTrue(element("screen.friends").waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Friends"].exists)
        app.buttons["Done"].tap()
        assertUsableHome()

        app.buttons["More"].tap()
        app.buttons["Settings"].tap()
        XCTAssertTrue(element("screen.settings").waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["CruiseMesh operation"].exists)
        app.buttons["Close"].tap()
        assertUsableHome()
    }

    func testFriendPreviewActionRemainsAvailableWithKeyboardOpen() {
        launch(scenario: "home-empty")
        app.buttons["Add a friend"].tap()
        XCTAssertTrue(element("screen.friends").waitForExistence(timeout: 5))

        let card = element("friends.card-input")
        XCTAssertTrue(card.waitForExistence(timeout: 3))
        card.tap()
        card.typeText("not-a-real-card")

        let keyboardAction = element("friends.preview-keyboard")
        XCTAssertTrue(keyboardAction.waitForExistence(timeout: 3))
        XCTAssertTrue(keyboardAction.isHittable)
    }

    func testComposerSendsOneVisibleMessageAndClearsDraft() {
        launch(scenario: "chat")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: 10))

        let bob = app.staticTexts["Bob"].firstMatch
        XCTAssertTrue(bob.waitForExistence(timeout: 10))
        if !bob.isHittable {
            app.swipeDown()
        }
        bob.tap()

        XCTAssertTrue(element("screen.chat").waitForExistence(timeout: 10))
        let composer = app.textViews["chat.composer.text"].exists
            ? app.textViews["chat.composer.text"]
            : element("chat.composer.text")
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText("   ")
        XCTAssertFalse(element("chat.composer.send").exists)
        composer.typeText("Hello from UI test")

        let send = element("chat.composer.send")
        XCTAssertTrue(send.waitForExistence(timeout: 10))
        XCTAssertTrue(send.isHittable)
        send.tap()

        let message = app.staticTexts["Hello from UI test"]
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        XCTAssertFalse(element("chat.composer.send").exists)
    }

    func testContactVerificationAndDeleteCancellationAreSafe() {
        launch(scenario: "chat")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: 10))
        app.staticTexts["Bob"].firstMatch.tap()
        XCTAssertTrue(element("screen.chat").waitForExistence(timeout: 5))

        element("chat.contact-details").tap()
        XCTAssertTrue(element("screen.contact-details").waitForExistence(timeout: 5))
        app.buttons["Verify contact"].tap()
        XCTAssertTrue(
            app.staticTexts["Match these words with your friend's screen to confirm it's really them."].exists
        )

        // Expanding verification pushes the destructive action below the
        // sheet's viewport. Scroll before asking XCTest for hittability: on
        // an off-screen SwiftUI button that query itself can fail the test.
        for _ in 0..<4 {
            app.swipeUp()
        }
        let delete = app.buttons["Delete contact"]
        XCTAssertTrue(delete.waitForExistence(timeout: 3))
        XCTAssertTrue(delete.isHittable)
        delete.tap()
        XCTAssertTrue(app.alerts["Delete contact?"].waitForExistence(timeout: 3))
        app.alerts["Delete contact?"].buttons["Cancel"].tap()
        XCTAssertTrue(element("screen.chat").waitForExistence(timeout: 3))
        XCTAssertFalse(app.alerts["Delete contact?"].exists)
    }

    private func launch(scenario: String) {
        self.scenario = scenario
        app = XCUIApplication()
        app.launchArguments = [
            "--ui-testing",
            "--ui-scenario", scenario,
            "-AppleLanguages", "(en)",
            "-AppleLocale", "en_US",
        ]
        app.launchEnvironment["CRUISEMESH_UI_TEST_RUN_ID"] = UUID().uuidString
        app.launch()
    }

    private func element(_ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    private func assertUsableHome(file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(
            element("screen.chat-list").waitForExistence(timeout: 10),
            "Home did not expose its root accessibility element",
            file: file,
            line: line
        )
        XCTAssertTrue(app.navigationBars["CruiseMesh"].exists, file: file, line: line)
        XCTAssertTrue(app.buttons["Add a friend"].isHittable, file: file, line: line)
    }
}
