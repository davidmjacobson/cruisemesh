import XCTest
@testable import CruiseMesh

final class VoicePlaybackDisplayTests: XCTestCase {
    func testABarShowingOnlyTheSenderDurationCannotBeSeeked() {
        XCTAssertNil(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: nil, fraction: 0.5))
        XCTAssertNil(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: 0, fraction: 0.5))
        XCTAssertNil(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: -1, fraction: 0.5))
    }

    func testADecoderDurationTurnsABarFractionIntoAClampedMillisecondTarget() {
        XCTAssertEqual(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: 10_000, fraction: 0), 0)
        XCTAssertEqual(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: 10_000, fraction: 0.5), 5_000)
        XCTAssertEqual(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: 10_000, fraction: 1), 10_000)
        XCTAssertEqual(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: 10_000, fraction: -2), 0)
        XCTAssertEqual(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: 10_000, fraction: 3), 10_000)
        XCTAssertNil(VoicePlaybackDisplay.seekTargetMs(decoderDurationMs: 10_000, fraction: .nan))
    }

    func testProgressIsZeroWhenTheTotalIsNotALength() {
        XCTAssertEqual(VoicePlaybackDisplay.progressFraction(positionMs: 4_000, totalMs: 0), 0)
        XCTAssertEqual(VoicePlaybackDisplay.progressFraction(positionMs: 4_000, totalMs: 16_000), 0.25, accuracy: 0.0001)
        XCTAssertEqual(VoicePlaybackDisplay.progressFraction(positionMs: -1, totalMs: 16_000), 0)
        XCTAssertEqual(VoicePlaybackDisplay.progressFraction(positionMs: 20_000, totalMs: 16_000), 1)
    }
}
