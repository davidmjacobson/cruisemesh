import XCTest
@testable import CruiseMesh

final class BackupCompatibilityTests: XCTestCase {
    func testSharedCmbakPayloadRoundTripsThroughIOSBindings() throws {
        let identity = generateIdentity()
        let payload = CoreBackupPayload(
            identity: encodeIdentityBytes(identity: identity),
            sqlite: Data([1, 2, 3, 4]),
            srcVersionCode: 17,
            createdAtMs: 1_700_000_000_000,
            displayName: "Alice",
            ownAvatar: Data([9, 8, 7]),
            ownAvatarEpoch: 3,
            relayUrl: "https://relay.example",
            relayToken: "secret",
            shareOnline: false
        )
        // Must be within the core's accepted PBKDF2 range (100_000..=1_200_000,
        // the T4-07 KDF-bomb guard); the minimum keeps the test fast. Mirrors
        // the Rust backup tests, which use PBKDF2_MIN_ITERATIONS.
        let file = try sealBackup(
            passphrase: "correct horse battery staple",
            payload: payload,
            iterations: 100_000
        )
        XCTAssertEqual(
            try openBackup(passphrase: "correct horse battery staple", file: file),
            payload
        )
    }
}
