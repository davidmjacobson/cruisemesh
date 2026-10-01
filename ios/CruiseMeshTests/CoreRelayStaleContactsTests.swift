import XCTest
@testable import CruiseMesh

/// The core relay engine writes a contact's rejection and silence streaks into
/// the store; the shell only has to report them. A friend whose family pass
/// lapsed must show up as *their* card being stale, never as this device's own
/// pass expiring (`HEALTH-01`), so the core pass path publishes these the same
/// way the legacy pass does.
///
/// Android reads the same streaks through `publishStaleContactRelays` after a
/// core pass; see `CoreRelayPassRunnerTest.kt`.
final class CoreRelayStaleContactsTests: XCTestCase {
    private let lapsedFriend = Data(repeating: 9, count: 32)
    private let onceRefused = Data(repeating: 8, count: 32)
    private let silentHost = Data(repeating: 7, count: 32)
    private let now: Int64 = 1_800_000_000_000

    func testOnlyStreaksAtCoresThresholdAreReportedStale() {
        let stale = MeshController.staleRelayContactIds(
            rejections: [
                ContactRelayRejection(userId: lapsedFriend, rejectStreak: 2, rejectedAtMs: now),
                ContactRelayRejection(userId: onceRefused, rejectStreak: 1, rejectedAtMs: now),
            ],
            unreachable: []
        )
        XCTAssertEqual(stale, [lapsedFriend])
        XCTAssertTrue(coreContactRelayIsStale(rejectStreak: 2))
        XCTAssertFalse(coreContactRelayIsStale(rejectStreak: 1))
    }

    func testSilenceStreaksAreReportedAlongsideRejections() {
        let threshold = (1...16).first { coreContactRelayUnreachableIsStale(unreachableStreak: Int64($0)) } ?? 1
        let stale = MeshController.staleRelayContactIds(
            rejections: [ContactRelayRejection(userId: lapsedFriend, rejectStreak: 2, rejectedAtMs: now)],
            unreachable: [
                ContactRelayUnreachable(
                    userId: silentHost,
                    endpointKey: relayCursorKey(relayUrl: "https://gone.example", relayToken: "tok"),
                    unreachableStreak: Int64(threshold),
                    unreachableAtMs: now
                ),
            ]
        )
        XCTAssertEqual(stale, [lapsedFriend, silentHost])
    }

    func testNothingStaleReportsNothing() {
        XCTAssertTrue(MeshController.staleRelayContactIds(rejections: [], unreachable: []).isEmpty)
    }
}
