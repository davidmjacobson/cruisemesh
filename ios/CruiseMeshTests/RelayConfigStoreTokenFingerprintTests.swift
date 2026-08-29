import XCTest
@testable import CruiseMesh

/// The relay token is a bearer credential and the diagnostics archive gets
/// mailed to whoever is helping. These pin the one place the pass is written
/// down: a digest of the token, never characters of it. Mirrors Android
/// `RelayConfigStoreTokenFingerprintTest`.
final class RelayConfigStoreTokenFingerprintTests: XCTestCase {

    private static let hexToken = "4ac9f24f8b1e4d7fae0c3b19d6725f88"
    private static let familyToken = "cmfam1-9d41c0b7e2a54f16"

    func testSamePassAlwaysGetsTheSameLabel() {
        // The whole reason to log anything: two lines, or two sessions, about
        // one pass have to be recognisable as the same pass.
        for token in [Self.hexToken, Self.familyToken] {
            XCTAssertEqual(
                RelayConfigStore.tokenFingerprint(token),
                RelayConfigStore.tokenFingerprint(token)
            )
        }
    }

    func testNoRunOfTheTokenSurvivesIntoTheLabel() {
        // What separates a digest from a truncation, and the property a future
        // "just shorten it" refactor would quietly break.
        for token in [Self.hexToken, Self.familyToken] {
            let fingerprint = RelayConfigStore.tokenFingerprint(token)
            let characters = Array(token)
            for width in 2...characters.count {
                for start in 0...(characters.count - width) {
                    let run = String(characters[start..<(start + width)])
                    XCTAssertFalse(
                        fingerprint.contains(run),
                        "fingerprint \(fingerprint) contains token run \(run)"
                    )
                }
            }
        }
    }

    func testTwoDifferentPassesStayDistinguishable() {
        // Telling a household's own pass apart from the shared tester pass in
        // a support hand-off.
        XCTAssertNotEqual(
            RelayConfigStore.tokenFingerprint(Self.hexToken),
            RelayConfigStore.tokenFingerprint(Self.familyToken)
        )
        // A pass that differs in one character has to land somewhere else
        // entirely; a prefix would have printed the same eight characters.
        XCTAssertNotEqual(
            RelayConfigStore.tokenFingerprint(Self.hexToken),
            RelayConfigStore.tokenFingerprint(String(Self.hexToken.dropLast()) + "9")
        )
    }

    func testLabelMatchesTheValueAndroidDerives() {
        // Pinned in the core's own tests too. Restated here because the point
        // of putting the derivation in the core is that a support person
        // comparing an iPhone archive against an Android one sees one pass
        // named once -- and without this, nothing here would fail if that
        // stopped being true.
        XCTAssertEqual(RelayConfigStore.tokenFingerprint(Self.hexToken), "056855d3")
        XCTAssertEqual(RelayConfigStore.tokenFingerprint(Self.familyToken), "6ae48e6b")
    }

    func testShortOrEmptyTokenDoesNotBlowUp() {
        XCTAssertEqual(RelayConfigStore.tokenFingerprint("abc").count, 8)
        XCTAssertEqual(RelayConfigStore.tokenFingerprint("").count, 8)
    }
}
