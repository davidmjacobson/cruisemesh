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
        let file = try sealBackup(
            passphrase: "correct horse battery staple",
            payload: payload,
            iterations: 10
        )
        XCTAssertEqual(
            try openBackup(passphrase: "correct horse battery staple", file: file),
            payload
        )
    }
}
