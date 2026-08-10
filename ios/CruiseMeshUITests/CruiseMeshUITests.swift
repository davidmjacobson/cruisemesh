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

    func testBackupPassphraseFieldsOfferRevealControls() {
        launch(scenario: "home-empty")
        assertUsableHome()

        app.buttons["More"].tap()
        app.buttons["Settings"].tap()
        XCTAssertTrue(element("screen.settings").waitForExistence(timeout: 5))

        let backup = app.staticTexts["Back up account"]
        for _ in 0..<5 where !backup.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(backup.isHittable)
        backup.tap()

        XCTAssertTrue(app.navigationBars["Back up account"].waitForExistence(timeout: 5))
        assertRevealControl("backup.export.passphrase.visibility")
        XCTAssertTrue(element("backup.export.passphrase").exists)
        XCTAssertTrue(element("backup.export.confirmation").exists)

        app.terminate()
        launch(scenario: "onboarding")
        let restore = app.buttons["Restore from backup"]
        XCTAssertTrue(restore.waitForExistence(timeout: 5))
        restore.tap()

        XCTAssertTrue(app.navigationBars["Restore from backup"].waitForExistence(timeout: 5))
        XCTAssertTrue(element("backup.restore.passphrase").exists)
        assertRevealControl("backup.restore.passphrase.visibility")
    }

    func testFriendPreviewActionRemainsAvailableWithKeyboardOpen() {
        launch(scenario: "home-empty")
        app.buttons["Add a friend"].tap()
        XCTAssertTrue(element("screen.friends").waitForExistence(timeout: 5))

        // Multi-line friend-card TextField + XCTest typeText is flaky on the
        // headless CI simulator (no keyboard focus after tap). UIPasteboard
        // paste also hangs the app idle wait. Seed pasteText and FocusState
        // through the UI-test-only control that uses the same bindings as the
        // human path, then assert the keyboard accessory is hittable.
        let seed = element("friends.uitest-seed-card")
        XCTAssertTrue(seed.waitForExistence(timeout: 5), "UI-test seed control missing")
        // Section sits under QR actions; scroll until the seed control is tappable.
        for _ in 0..<6 where !seed.isHittable {
            app.swipeUp()
        }
        seed.tap()

        let keyboardAction = element("friends.preview-keyboard")
        XCTAssertTrue(
            keyboardAction.waitForExistence(timeout: 8),
            "Keyboard accessory Preview friend should appear once the card field is focused with text"
        )
        XCTAssertTrue(keyboardAction.isHittable)
    }

    func testComposerSendsOneVisibleMessageAndClearsDraft() {
        launch(scenario: "chat")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: 10))

        let bob = app.staticTexts["Bob"].firstMatch
        XCTAssertTrue(bob.waitForExistence(timeout: 10))
        bob.tap()

        let composer = element("chat.composer.text").waitForExistence(timeout: 10)
            ? element("chat.composer.text")
            : (app.textFields.firstMatch.waitForExistence(timeout: 5)
                ? app.textFields.firstMatch
                : app.textViews.firstMatch)
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText("   ")
        XCTAssertFalse(element("chat.composer.send").exists)
        composer.tap()
        composer.typeText("Hello from UI test")

        let send = app.buttons["chat.composer.send"].waitForExistence(timeout: 10)
            ? app.buttons["chat.composer.send"]
            : (app.buttons["Send"].waitForExistence(timeout: 5)
                ? app.buttons["Send"]
                : element("chat.composer.send"))
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

    func testChatListMarkReadAndDeleteRequireDeliberateActions() {
        launch(scenario: "chat-list-actions")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: 10))
        let dad = app.staticTexts["Dad"].firstMatch
        XCTAssertTrue(dad.waitForExistence(timeout: 5), "Saved nickname should label the chat row")

        dad.swipeLeft()
        let markRead = app.buttons["Mark as read"]
        XCTAssertTrue(markRead.waitForExistence(timeout: 3))
        markRead.tap()

        dad.swipeLeft()
        XCTAssertFalse(app.buttons["Mark as read"].exists)
        let delete = app.buttons["Delete"]
        XCTAssertTrue(delete.waitForExistence(timeout: 3))
        delete.tap()
        XCTAssertTrue(app.alerts["Delete Dad?"].waitForExistence(timeout: 3))
        app.alerts["Delete Dad?"].buttons["Cancel"].tap()
        XCTAssertTrue(dad.exists)
        attachScreenshot(named: "Chat-list-actions-nickname-mark-read-delete-cancel")
    }

    func testIncomingMessageWhileReadingHistoryShowsUsableJumpAction() {
        launch(scenario: "chat-late-arrival")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: 10))
        app.staticTexts["Bob"].firstMatch.tap()
        XCTAssertTrue(element("screen.chat").waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["History message 32"].waitForExistence(timeout: 5))

        for _ in 0..<4 { app.swipeDown() }
        let inject = element("chat.uitest-inject-incoming")
        XCTAssertTrue(inject.waitForExistence(timeout: 3))
        inject.tap()

        // Assert the control through the name VoiceOver users interact with.
        // SwiftUI exposes this transient overlay as a labelled button on the
        // current SDK, but does not propagate its view identifier into the
        // accessibility snapshot.
        let jump = app.buttons["New messages"].firstMatch
        XCTAssertTrue(jump.waitForExistence(timeout: 5))
        XCTAssertTrue(jump.isHittable)
        attachScreenshot(named: "Chat-new-messages-action")
        jump.tap()
        XCTAssertTrue(app.staticTexts["New message while reading history"].waitForExistence(timeout: 5))
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

    private func attachScreenshot(named name: String) {
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = name
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }

    private func assertRevealControl(
        _ identifier: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let reveal = element(identifier)
        for _ in 0..<5 where !reveal.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(reveal.waitForExistence(timeout: 5), file: file, line: line)
        XCTAssertTrue(reveal.isHittable, file: file, line: line)
        XCTAssertEqual(reveal.label, "Show passphrase", file: file, line: line)
        reveal.tap()
        XCTAssertEqual(reveal.label, "Hide passphrase", file: file, line: line)
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
