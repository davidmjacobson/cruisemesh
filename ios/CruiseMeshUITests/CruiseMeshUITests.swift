import Foundation
import XCTest

final class CruiseMeshUITests: XCTestCase {
    private var app: XCUIApplication!
    private var scenario = ""

    /// Waits here are about a busy shared CI runner, not about product latency:
    /// the simulator shares a machine with a build, so an element that appears
    /// instantly by hand can take seconds there. Short timeouts made this suite
    /// fail on a different test every run. A too-generous timeout costs nothing
    /// when the element does appear; a too-tight one reds the only gate iOS has.
    private static let uiTimeout: TimeInterval = 10

    /// Mirrors the message count the late-arrival fixture seeds. Kept in step
    /// with `UITestConfiguration`; the fixture is deep enough to overflow the
    /// tallest simulator screen, which is what lets a test scroll away from
    /// the bottom of the thread on every device shape.
    private static let seededHistoryCount = 120

    /// Shorter than the general timeout on purpose: the keyboard is only a
    /// hint that focus landed, and a run that never gets one must not spend
    /// the full wait twice per field.
    private static let keyboardTimeout: TimeInterval = 5

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
        XCUIDevice.shared.orientation = .portrait
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
        focusAndType(name, "UI Tester")

        let start = app.buttons["Start using CruiseMesh"]
        XCTAssertTrue(start.isEnabled)
        start.tap()

        assertUsableHome()
    }

    func testOnboardingPrimaryActionsStayVisibleInCompactLandscape() {
        launch(scenario: "onboarding")
        XCTAssertTrue(element("screen.onboarding").waitForExistence(timeout: 10))

        XCUIDevice.shared.orientation = .landscapeLeft
        for _ in 0..<4 {
            let next = app.buttons["Next"]
            XCTAssertTrue(next.waitForExistence(timeout: 3))
            XCTAssertTrue(next.isHittable, "Next must remain visible outside the scrolling page content")
            next.tap()
        }

        let start = app.buttons["Start using CruiseMesh"]
        XCTAssertTrue(start.waitForExistence(timeout: 3))
        XCTAssertTrue(start.isHittable, "The final primary action must remain visible in landscape")
        XCTAssertTrue(app.buttons["Restore from backup"].isHittable)
    }

    func testNewGroupMemberSelectionAnnouncesItsState() {
        launch(scenario: "chat")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: 10))

        app.buttons["New chat"].tap()
        app.buttons["New group"].tap()

        let bob = app.buttons["Bob"].firstMatch
        XCTAssertTrue(bob.waitForExistence(timeout: 5))
        XCTAssertEqual(bob.value as? String, "Not selected")
        bob.tap()
        XCTAssertEqual(bob.value as? String, "Selected")
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
        XCTAssertTrue(scrollUntilHittable(backup), "Back up account never came into view")
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
        // The seed control sits in the fourth section, below the QR actions.
        // A List builds its rows lazily, so on a small screen at a large
        // accessibility text size that row is not merely off-screen: it is
        // absent from the accessibility tree entirely until it is scrolled
        // near. Waiting for existence *before* scrolling therefore always
        // times out on those device shapes, which is exactly what the nightly
        // compact profile has been reporting. Scroll and re-query together.
        let seed = element("friends.uitest-seed-card")
        XCTAssertTrue(
            scrollUntilHittable(seed),
            "UI-test seed control never came into view"
        )
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
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: Self.uiTimeout))
        openChat(named: "Bob")

        let composer = element("chat.composer.text").waitForExistence(timeout: 10)
            ? element("chat.composer.text")
            : (app.textFields.firstMatch.waitForExistence(timeout: 5)
                ? app.textFields.firstMatch
                : app.textViews.firstMatch)
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        focusAndType(composer, "   ")
        XCTAssertFalse(element("chat.composer.send").exists)
        focusAndType(composer, "Hello from UI test")

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

    func testRecipientNameStaysVisibleWhileComposing() {
        launch(scenario: "chat")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: Self.uiTimeout))
        openChat(named: "Bob")

        let composer = element("chat.composer.text").waitForExistence(timeout: 10)
            ? element("chat.composer.text")
            : (app.textFields.firstMatch.waitForExistence(timeout: 5)
                ? app.textFields.firstMatch
                : app.textViews.firstMatch)
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        focusAndType(composer, "Checking the header")

        let recipientHeader = element("chat.contact-details")
        XCTAssertTrue(recipientHeader.waitForExistence(timeout: 5))
        XCTAssertTrue(recipientHeader.isHittable)
        XCTAssertTrue(recipientHeader.label.contains("Bob"))
    }

    func testScrubbingAVoiceMessageDoesNotStartAReply() {
        launch(scenario: "chat")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: Self.uiTimeout))
        openChat(named: "Bob")
        XCTAssertTrue(element("screen.chat").waitForExistence(timeout: Self.uiTimeout))

        let seek = element("voice.seek")
        XCTAssertTrue(
            seek.waitForExistence(timeout: Self.uiTimeout),
            "The seeded voice message should expose a seek bar"
        )

        // Travel well past the 56 pt swipe-to-reply threshold. Before the
        // seek bar claimed the drag, this started a reply.
        let start = seek.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.5))
        let end = seek.coordinate(withNormalizedOffset: CGVector(dx: 2.5, dy: 0.5))
        start.press(forDuration: 0.05, thenDragTo: end)

        XCTAssertFalse(
            element("chat.reply-preview").exists,
            "A rightward scrub on the seek bar must not start a swipe-to-reply"
        )
        XCTAssertFalse(app.buttons["Cancel reply"].exists)
    }

    func testContactVerificationAndDeleteCancellationAreSafe() {
        launch(scenario: "chat")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: Self.uiTimeout))
        openChat(named: "Bob")

        let details = element("chat.contact-details")
        XCTAssertTrue(details.waitForExistence(timeout: Self.uiTimeout))
        details.tap()
        XCTAssertTrue(element("screen.contact-details").waitForExistence(timeout: Self.uiTimeout))

        let verify = app.buttons["Verify contact"]
        XCTAssertTrue(verify.waitForExistence(timeout: Self.uiTimeout))
        verify.tap()
        XCTAssertTrue(
            app.staticTexts["Match these words with your friend's screen to confirm it's really them."]
                .waitForExistence(timeout: Self.uiTimeout)
        )

        // Expanding verification pushes the destructive action below the
        // sheet's viewport, and the sheet builds its rows lazily, so an
        // unscrolled query can miss the button outright rather than merely
        // find it unhittable. Scroll and re-query together.
        let delete = app.buttons["Delete contact"]
        XCTAssertTrue(
            scrollUntilHittable(delete),
            "The delete action never came into view on the contact sheet"
        )
        delete.tap()
        let alert = app.alerts["Delete contact?"]
        XCTAssertTrue(alert.waitForExistence(timeout: Self.uiTimeout))
        let cancel = alert.buttons["Cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: Self.uiTimeout))
        cancel.tap()
        XCTAssertTrue(element("screen.chat").waitForExistence(timeout: Self.uiTimeout))
        XCTAssertTrue(alert.waitForNonExistence(timeout: Self.uiTimeout))
    }

    func testChatListMarkReadAndDeleteRequireDeliberateActions() {
        launch(scenario: "chat-list-actions")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: Self.uiTimeout))
        let dad = app.staticTexts["Dad"].firstMatch
        XCTAssertTrue(
            dad.waitForExistence(timeout: Self.uiTimeout),
            "Saved nickname should label the chat row"
        )

        revealSwipeAction(inRowLabeled: "Dad", named: "Mark as read").tap()
        // Wait for the first swipe chrome to dismiss. CI has hung here
        // treating the envelope button as an interrupting element.
        XCTAssertTrue(app.buttons["Mark as read"].waitForNonExistence(timeout: Self.uiTimeout))

        let delete = revealSwipeAction(inRowLabeled: "Dad", named: "Delete")
        XCTAssertFalse(app.buttons["Mark as read"].exists)
        delete.tap()
        let alert = app.alerts["Delete Dad?"]
        XCTAssertTrue(alert.waitForExistence(timeout: Self.uiTimeout))
        let cancel = alert.buttons["Cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: Self.uiTimeout))
        cancel.tap()
        XCTAssertTrue(dad.exists)
        attachScreenshot(named: "Chat-list-actions-nickname-mark-read-delete-cancel")
    }

    func testIncomingMessageWhileReadingHistoryShowsUsableJumpAction() {
        launch(scenario: "chat-late-arrival")
        XCTAssertTrue(element("screen.chat-list").waitForExistence(timeout: Self.uiTimeout))
        openChat(named: "Bob")
        let newest = app.staticTexts["History message \(Self.seededHistoryCount)"]
        XCTAssertTrue(newest.waitForExistence(timeout: Self.uiTimeout))

        // Scrolling away from the newest message is this test's precondition,
        // not scene-setting: the jump action only exists while the reader is
        // somewhere above the bottom of the thread. A fixed number of swipes
        // proved nothing — on a large screen the whole seeded history fits at
        // once, so every swipe bounced, the thread stayed at the bottom, and
        // the failure surfaced much later as "the jump action never appeared".
        // Scroll until the newest message is genuinely out of reach, and say
        // so here if it never is.
        XCTAssertTrue(
            scrollUntilOutOfView(newest),
            "The newest message stayed on screen, so the thread never entered the reading-history state this test is about"
        )

        let inject = element("chat.uitest-inject-incoming")
        XCTAssertTrue(inject.waitForExistence(timeout: Self.uiTimeout))
        inject.tap()

        // Assert the control through the name VoiceOver users interact with,
        // and accept the view identifier too: SwiftUI does not always
        // propagate an overlay's identifier into the accessibility snapshot,
        // and which handle survives has changed between SDKs.
        let labelled = app.buttons["New messages"].firstMatch
        XCTAssertTrue(
            labelled.waitForExistence(timeout: Self.uiTimeout)
                || element("chat.new-messages").waitForExistence(timeout: Self.uiTimeout),
            "No jump-to-new-messages action appeared after a message arrived"
        )
        let jump = labelled.exists ? labelled : element("chat.new-messages")
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

    /// Scrolls down until `target` is both present and tappable.
    ///
    /// Existence and visibility are the same question in a SwiftUI `List`,
    /// which builds its rows lazily: a row far below the fold is absent from
    /// the accessibility tree, so `waitForExistence` on it can only ever time
    /// out no matter how generous the timeout. That is why the wait has to
    /// happen *inside* the scroll rather than before it — and why the nightly
    /// profiles, which shrink the screen or enlarge the text and so push more
    /// rows below the fold, failed where the default phone passed.
    private func scrollUntilHittable(_ target: XCUIElement, maxSwipes: Int = 8) -> Bool {
        if target.waitForExistence(timeout: 2), target.isHittable { return true }
        for _ in 0..<maxSwipes {
            app.swipeUp()
            // A short wait after each swipe lets the list build the rows the
            // swipe just revealed before the next query asks about them.
            if target.waitForExistence(timeout: 1), target.isHittable { return true }
        }
        return target.exists && target.isHittable
    }

    /// Scrolls up (back through history) until `target` is off screen.
    ///
    /// Returns false rather than swiping forever, so a caller can fail with a
    /// reason instead of blaming whatever it checked next.
    private func scrollUntilOutOfView(_ target: XCUIElement, maxSwipes: Int = 8) -> Bool {
        for _ in 0..<maxSwipes {
            if !target.exists || !target.isHittable { return true }
            app.swipeDown()
        }
        return !target.exists || !target.isHittable
    }

    /// Taps a text entry and waits for the keyboard before typing into it.
    ///
    /// `typeText` straight after a tap fails outright with "neither element
    /// nor any descendant has keyboard focus" when a loaded runner has not
    /// finished raising the keyboard — a real failure seen on the nightly
    /// suite. Waiting for the keyboard, and re-tapping once if it never came,
    /// removes the race without hiding a genuinely unfocusable field.
    ///
    /// The keyboard is treated as a hint, not a verdict: a simulator with a
    /// hardware keyboard attached can accept typing without ever showing one,
    /// so a missing keyboard buys a second tap and then gets out of the way.
    /// If the field really cannot take input, `typeText` still says so.
    private func focusAndType(_ field: XCUIElement, _ text: String) {
        field.tap()
        if !app.keyboards.element.waitForExistence(timeout: Self.keyboardTimeout) {
            field.tap()
            _ = app.keyboards.element.waitForExistence(timeout: Self.keyboardTimeout)
        }
        field.typeText(text)
    }

    /// Taps a chat-list row and waits for the thread to actually be on screen.
    ///
    /// Tests used to tap the row and go straight to querying the composer, so a
    /// tap the list swallowed while it was still settling surfaced much later
    /// as "the composer does not exist" — a confusing failure a long way from
    /// its cause. Assert the navigation happened, and retry the tap once.
    private func openChat(
        named contact: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let row = app.staticTexts[contact].firstMatch
        XCTAssertTrue(
            row.waitForExistence(timeout: Self.uiTimeout),
            "No chat row for \(contact)",
            file: file,
            line: line
        )
        row.tap()
        let chat = element("screen.chat")
        if chat.waitForExistence(timeout: Self.uiTimeout) { return }
        app.staticTexts[contact].firstMatch.tap()
        XCTAssertTrue(
            chat.waitForExistence(timeout: Self.uiTimeout),
            "Tapping \(contact) did not open the chat thread",
            file: file,
            line: line
        )
    }

    /// Reveals a trailing swipe action on a chat-list row and returns it.
    ///
    /// Both halves matter on CI. The row is re-queried on every attempt: a row
    /// captured before a swipe can refer to a cell the list has since rebuilt,
    /// and swiping it does nothing visible. And the swipe itself is retried,
    /// because a single dropped gesture on a loaded runner was reliably reading
    /// as "the app never offered Delete". Safe to repeat: the list declares
    /// `allowsFullSwipe: false`, so an extra swipe can never fire the
    /// destructive action on its own.
    @discardableResult
    private func revealSwipeAction(
        inRowLabeled label: String,
        named action: String,
        attempts: Int = 3,
        file: StaticString = #filePath,
        line: UInt = #line
    ) -> XCUIElement {
        for attempt in 0..<attempts {
            if element("screen.chat").exists, !element("screen.chat-list").exists {
                app.navigationBars.buttons.firstMatch.tap()
                _ = element("screen.chat-list").waitForExistence(timeout: Self.uiTimeout)
            }
            let row = app.cells.containing(.staticText, identifier: label).firstMatch
            guard row.waitForExistence(timeout: Self.uiTimeout) else { continue }
            row.swipeLeft(velocity: attempt == 0 ? .slow : XCUIGestureVelocity(250))
            let button = app.buttons[action]
            if button.waitForExistence(timeout: Self.uiTimeout), button.isHittable {
                return button
            }
        }
        XCTFail(
            "Swiping the '\(label)' row never revealed '\(action)' in \(attempts) attempts",
            file: file,
            line: line
        )
        return app.buttons[action]
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
        XCTAssertTrue(scrollUntilHittable(reveal), file: file, line: line)
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
