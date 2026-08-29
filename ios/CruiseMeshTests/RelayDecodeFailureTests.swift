import XCTest
@testable import CruiseMesh

/// What the log is allowed to say when a relay answers 200 with a body the
/// core decoder refuses.
///
/// Whatever answered a relay call is not necessarily the relay: a captive
/// portal, a hotel proxy, a gateway. Its bytes are the one part of the
/// exchange nobody here chose, and this app can export its log into an archive
/// a user shares. So the line describes the failure — its shape, where it
/// stopped, how big the body was — and reproduces none of it.
///
/// Mirrors `RelayClientTest."a body that will not decode is described without
/// quoting it"` on Android.
final class RelayDecodeFailureTests: XCTestCase {

    private static let marker = "MARKER-cabin-8042"

    /// A well-formed JSON document that is not a fetch page. `serde_json`
    /// would have quoted the offending value into its own message; the core's
    /// `json_fault` replaces it with the category and the position.
    func testAValueTheDecoderRejectedIsNotReproduced() throws {
        let body = Data(#"{"id":"\#(Self.marker)"}"#.utf8)
        XCTAssertEqual(body.count, 26)
        let detail = try decodeFailureDetail(for: body)
        assertDescribes(detail, body, "data error at line 1 column 26 of 26B")
    }

    /// A sign-in page served with a 200. The status branch never sees this
    /// one, so this line is all the reader gets — and it still has to be
    /// enough to act on: not JSON at all, refused at the first byte, 57 bytes
    /// of it.
    func testASignInPageServedWithA200IsNamedByShapeAndSize() throws {
        let body = Data("<html><title>\(Self.marker) Guest Wi-Fi</title></html>".utf8)
        XCTAssertEqual(body.count, 57)
        let detail = try decodeFailureDetail(for: body)
        assertDescribes(detail, body, "syntax error at line 1 column 1 of 57B")
    }

    /// A body cut in half by a link that gave out reads as `eof`, not as a
    /// disagreement about the page — the distinction that says whether the
    /// same window is worth retrying or has to be shrunk.
    func testATruncatedPageIsAnEofFailure() throws {
        let body = Data(#"{"envelopes":[],"next_curs"#.utf8)
        let detail = try decodeFailureDetail(for: body)
        assertDescribes(detail, body, "eof error at line 1 column 26 of 26B")
    }

    /// The whole sentence, pinned exactly — the same standard the core's own
    /// tests and Android's `RelayClientTest` hold this line to. "Does not
    /// contain the body" is the property, but only equality rules out a later
    /// edit appending something else that came off the wire.
    ///
    /// UniFFI wraps the core message rather than merely prefixing it: it
    /// conforms `CoreError` to `LocalizedError` with `String(reflecting:
    /// self)`, so the rendered case is `CruiseMesh.CoreError.Malformed("…")`
    /// — a module-qualified head *and* a closing `")` after the message. That
    /// middle is the generator's formatting rather than this app's text, and
    /// pinning it does tie this test to the binding generator; that is the
    /// deliberate trade, because the alternative left the tail of the sentence
    /// unpinned, which is precisely where appended wire content would land.
    /// If a UniFFI upgrade changes the rendering, this is the assertion to
    /// re-read against the new output — never to loosen.
    ///
    /// The marker check is kept alongside it. Equality already implies it, but
    /// it states the property this file exists for and, failing first, says so
    /// in the failure message instead of leaving a reader to diff two long
    /// strings.
    private func assertDescribes(
        _ detail: String,
        _ body: Data,
        _ coreMessage: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertFalse(
            detail.contains(Self.marker),
            "the response body reached the log: \(detail)",
            file: file,
            line: line
        )
        XCTAssertEqual(
            detail,
            """
            could not decode \(body.count)B: \
            CruiseMesh.CoreError.Malformed("invalid relay JSON: \(coreMessage)")
            """,
            file: file,
            line: line
        )
    }

    private func decodeFailureDetail(for body: Data) throws -> String {
        do {
            _ = try relayDecodeFetchPage(body: body)
            XCTFail("expected the decoder to refuse this body")
            return ""
        } catch {
            return RelayClient.decodeFailureDetail(bytes: body.count, error: error)
        }
    }
}
