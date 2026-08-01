import SwiftUI
import XCTest
@testable import CruiseMesh

/// 6.6: the bubble styles the core's UTF-16 ranges in place. The mapping from
/// those offsets to `AttributedString` indices is the only part that can be
/// silently *wrong* on a real message -- one emoji ahead of a link shifts
/// every offset -- so these tests assert the rendered characters of each
/// linked run are the destination, character for character.
final class MessageBodyTextTests: XCTestCase {
    private func links(_ body: String) -> [(text: String, url: URL)] {
        let attributed = MessageBodyText.attributedBody(text: body, linkColor: .accentColor)
        var found: [(text: String, url: URL)] = []
        for run in attributed.runs {
            guard let url = run.link else { continue }
            found.append((String(attributed[run.range].characters), url))
        }
        return found
    }

    func testPlainTextHasNoLinks() {
        XCTAssertTrue(links("meet at deck 9 at 7").isEmpty)
        XCTAssertTrue(links("").isEmpty)
    }

    func testVisibleTextIsTheDestination() {
        let found = links("Set up here: https://cruisemesh.app/r/#CMRELAY1:ab-c_d, then reply.")
        XCTAssertEqual(found.count, 1)
        XCTAssertEqual(found.first?.text, "https://cruisemesh.app/r/#CMRELAY1:ab-c_d")
        XCTAssertEqual(
            found.first?.url.absoluteString,
            "https://cruisemesh.app/r/#CMRELAY1:ab-c_d"
        )
    }

    /// An astral character is two UTF-16 code units. If the offsets were
    /// treated as Rust byte offsets or as Swift `Character` counts, the run
    /// would slide and this would fail rather than crash in someone's chat.
    func testOffsetsSurviveEmoji() {
        let found = links("🎉 https://x.example 🎉")
        XCTAssertEqual(found.count, 1)
        XCTAssertEqual(found.first?.text, "https://x.example")
    }

    func testEveryLinkInABodyIsStyled() {
        let found = links("https://a.example and https://b.example")
        XCTAssertEqual(found.map(\.text), ["https://a.example", "https://b.example"])
    }

    /// The shell must not second-guess the core's allow-list, and the core
    /// refuses these -- so nothing here becomes tappable.
    func testRefusedSchemesStayPlainText() {
        XCTAssertTrue(links("javascript:alert(1)").isEmpty)
        XCTAssertTrue(links("http://example.com").isEmpty)
        XCTAssertTrue(links("cruisemesh.app").isEmpty)
    }

    /// Both of these render as the single address
    /// `https://evil.example.apple.example` -- a soft hyphen draws nothing, a
    /// one-dot leader draws a full stop -- while only `https://evil.example`
    /// would be underlined and opened. The core refuses them outright; this
    /// pins that the refusal survives the FFI boundary.
    func testAHiddenBoundaryLeavesNoTappablePrefix() {
        XCTAssertTrue(links("https://evil.example\u{00AD}.apple.example").isEmpty)
        XCTAssertTrue(links("https://evil.example\u{2024}apple.example").isEmpty)
    }

    func testCruiseMeshLinksAreTappable() {
        let found = links("open cruisemesh://f#CARD")
        XCTAssertEqual(found.count, 1)
        XCTAssertEqual(found.first?.text, "cruisemesh://f#CARD")
    }
}
