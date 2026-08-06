import XCTest
@testable import CruiseMesh

final class FamilyRelayBackpressureTests: XCTestCase {
    func testRequestPacerCapsPhoneAtTwoRequestsPerSecond() {
        let pacer = FamilyRelayRequestPacer()

        XCTAssertEqual(pacer.reserve(nowMs: 10_000), 0)
        XCTAssertEqual(pacer.reserve(nowMs: 10_000), 500)
        XCTAssertEqual(pacer.reserve(nowMs: 10_250), 750)
        XCTAssertEqual(pacer.reserve(nowMs: 12_000), 0)
    }

    func testThreeFamilyClientsRecoverOnStaggeredDeadlines() {
        let identities = [Data([1]), Data([2]), Data([3])]
        let clients = identities.map { _ in FamilyRelayBackoff() }

        let firstRetryDelays = zip(clients, identities).map { pair in
            let (client, identity) = pair
            return client.onRateLimited(
                retryAfterMs: 1_000,
                identityHash: familyRelayIdentityHash(identity)
            )
        }
        XCTAssertEqual(Set(firstRetryDelays).count, 3)
        XCTAssertTrue(firstRetryDelays.allSatisfy { (1_000...2_000).contains($0) })

        let secondRetryDelays = zip(clients, identities).map { pair in
            let (client, identity) = pair
            return client.onRateLimited(
                retryAfterMs: 1_000,
                identityHash: familyRelayIdentityHash(identity)
            )
        }
        XCTAssertTrue(secondRetryDelays.allSatisfy { $0 >= 2_000 })

        clients.forEach { $0.onSuccessfulPass() }
        let recoveredRetryDelays = zip(clients, identities).map { pair in
            let (client, identity) = pair
            return client.onRateLimited(
                retryAfterMs: 1_000,
                identityHash: familyRelayIdentityHash(identity)
            )
        }
        XCTAssertEqual(firstRetryDelays, recoveredRetryDelays)
    }

    func testServerRetryAfterRemainsMinimumQuietPeriod() {
        let delayMs = familyRelayBackoffDelayMs(
            retryAfterMs: 15_000,
            consecutiveRateLimits: 1,
            identityHash: 42
        )

        XCTAssertTrue((15_000...16_000).contains(delayMs))
    }

    func testExponentialRetryIsCappedBeforeJitter() {
        XCTAssertEqual(
            familyRelayBackoffDelayMs(
                retryAfterMs: 1_000,
                consecutiveRateLimits: 100,
                identityHash: 0
            ),
            familyRelayBackoffCapMs
        )
    }

    func testIdentityJitterHashIsStable() {
        let identity = Data([0x01, 0x02, 0x03, 0x04])
        XCTAssertEqual(familyRelayIdentityHash(identity), familyRelayIdentityHash(identity))
        XCTAssertNotEqual(familyRelayIdentityHash(identity), familyRelayIdentityHash(Data([0x04, 0x03, 0x02, 0x01])))
    }
}
