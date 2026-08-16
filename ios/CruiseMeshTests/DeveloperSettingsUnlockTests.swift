import XCTest
@testable import CruiseMesh

/// The seven-tap run, pinned to the same rule Android's counter follows: seven
/// deliberate taps open the door, a stalled run does not, and nothing is said
/// until the fourth tap so an accidental double-tap stays invisible.
final class DeveloperSettingsUnlockTests: XCTestCase {

    func testSevenTapsInARowReachTheThreshold() {
        let counter = DeveloperSettingsTapCounter()
        let seen = (1...7).map { counter.tap(at: Double($0) * 0.1) }

        XCTAssertEqual(
            seen,
            [
                .quiet,
                .quiet,
                .quiet,
                .countdown(remaining: 3),
                .countdown(remaining: 2),
                .countdown(remaining: 1),
                .reached,
            ]
        )
    }

    func testReachingTheThresholdStartsTheNextRunFromZero() {
        let counter = DeveloperSettingsTapCounter()
        for i in 0..<7 { _ = counter.tap(at: Double(i) * 0.1) }

        // An eighth tap is the first of a fresh run, not an eighth of the old
        // one -- otherwise every further tap would toggle the flag again.
        XCTAssertEqual(counter.tap(at: 0.8), .quiet)
    }

    func testAPauseLongerThanTheWindowStartsTheCountOver() {
        let counter = DeveloperSettingsTapCounter()
        for i in 0..<6 { _ = counter.tap(at: Double(i) * 0.1) }

        XCTAssertEqual(counter.tap(at: 0.5 + developerSettingsTapWindow + 0.001), .quiet)
    }

    func testTapsAtTheEdgeOfTheWindowStillContinueTheRun() {
        let counter = DeveloperSettingsTapCounter()
        var now: TimeInterval = 0
        for _ in 0..<(developerSettingsUnlockTaps - 1) {
            _ = counter.tap(at: now)
            now += developerSettingsTapWindow
        }

        XCTAssertEqual(counter.tap(at: now), .reached)
    }

    func testAClockThatJumpsBackwardsStartsTheCountOver() {
        let counter = DeveloperSettingsTapCounter()
        for i in 0..<6 { _ = counter.tap(at: 10_000 + Double(i) * 0.1) }

        XCTAssertEqual(counter.tap(at: 9_000), .quiet)
    }

    func testAnExplicitResetAbandonsARunInProgress() {
        let counter = DeveloperSettingsTapCounter()
        for i in 0..<6 { _ = counter.tap(at: Double(i) * 0.1) }
        counter.reset()

        XCTAssertEqual(counter.tap(at: 0.7), .quiet)
    }

    func testTheVersionRowSaysWhatEachTapDid() {
        XCTAssertEqual(developerSettingsLabel(for: .quiet, unlockedAfterTap: false), .version)
        XCTAssertEqual(
            developerSettingsLabel(for: .countdown(remaining: 3), unlockedAfterTap: false),
            .countdown(remaining: 3)
        )
        XCTAssertEqual(developerSettingsLabel(for: .reached, unlockedAfterTap: true), .unlocked)
        XCTAssertEqual(developerSettingsLabel(for: .reached, unlockedAfterTap: false), .hidden)
    }

    func testAnEarlyTapClearsFeedbackLeftOverFromAnAbandonedRun() {
        // Taps one to three of a fresh run put the version string back, so a
        // count from a run that was given up on cannot linger under a new one.
        XCTAssertEqual(developerSettingsLabel(for: .quiet, unlockedAfterTap: true), .version)
    }

    func testFeedbackClearsBeforeTheRunItBelongsToExpires() {
        // The row goes quiet while a run may still be live, never the other way
        // round: a count that outlived its own run would be a lie on screen.
        XCTAssertLessThan(developerSettingsLabelRevert, developerSettingsTapWindow)
    }

    /// A debug build shows the entry without any unlock; a release build shows
    /// it only once the flag is set. The warning is the other way round: it is
    /// for the release case only.
    func testVisibilityAndWarningFollowTheBuildType() {
        let wasUnlocked = DeveloperSettingsUnlockStore.isUnlocked()
        defer { DeveloperSettingsUnlockStore.setUnlocked(wasUnlocked) }

        DeveloperSettingsUnlockStore.setUnlocked(false)
        XCTAssertEqual(developerSettingsVisible, developerSettingsDebugBuild)
        XCTAssertFalse(developerSettingsUnlockedOnRelease)

        DeveloperSettingsUnlockStore.setUnlocked(true)
        XCTAssertTrue(developerSettingsVisible)
        XCTAssertEqual(developerSettingsUnlockedOnRelease, !developerSettingsDebugBuild)
    }
}
