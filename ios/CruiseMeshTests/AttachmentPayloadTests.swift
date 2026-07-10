import XCTest
@testable import CruiseMesh

final class AttachmentPayloadTests: XCTestCase {
    func testRoundTripImage() {
        let original = AttachmentPayload(
            mediaType: .image,
            mimeType: "image/jpeg",
            durationMs: 0,
            blob: Data([1, 2, 3, 4, 5]),
            caption: "pool"
        )
        let decoded = AttachmentPayload.decode(original.encode())
        XCTAssertEqual(decoded, original)
    }

    func testRoundTripAudio() {
        let original = AttachmentPayload(
            mediaType: .audio,
            mimeType: "audio/mp4",
            durationMs: 12_345,
            blob: Data((0..<64).map { UInt8($0) })
        )
        let decoded = AttachmentPayload.decode(original.encode())
        XCTAssertEqual(decoded?.mediaType, .audio)
        XCTAssertEqual(decoded?.durationMs, 12_345)
        XCTAssertEqual(decoded?.blob, original.blob)
    }

    func testRejectsUnknownVersion() {
        var bytes = AttachmentPayload(
            mediaType: .image,
            mimeType: "image/jpeg",
            durationMs: 0,
            blob: Data([9])
        ).encode()
        bytes[0] = 99
        XCTAssertNil(AttachmentPayload.decode(bytes))
    }

    func testRejectsTruncatedNumericFields() {
        XCTAssertNil(AttachmentPayload.decode(Data([1, 1, 0, 0, 0, 0, 0])))
    }

    func testPreviewLabels() {
        let photo = AttachmentPayload(mediaType: .image, mimeType: "image/jpeg", durationMs: 0, blob: Data([1]))
        XCTAssertEqual(AttachmentPayload.previewLabel(photo), "📷 Photo")
        let voice = AttachmentPayload(mediaType: .audio, mimeType: "audio/mp4", durationMs: 4_200, blob: Data([1]))
        XCTAssertTrue(AttachmentPayload.previewLabel(voice).hasPrefix("🎤 Voice memo"))
    }
}
