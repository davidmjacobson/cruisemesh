import AVFoundation
import XCTest
@testable import CruiseMesh

final class VoiceMemoPlaybackTests: XCTestCase {
    func testPlaybackSuppliesM4ATypeHintAndOwnsAudioSessionUntilCompletion() {
        let fake = FakeVoiceMemoAudioPlayer()
        let blob = Data([1, 2, 3])
        var receivedBlob: Data?
        var receivedHint: String?
        var activations = 0
        var deactivations = 0
        let playback = VoiceMemoPlaybackController(
            playerFactory: { data, hint in
                receivedBlob = data
                receivedHint = hint
                return fake
            },
            activateAudioSession: { activations += 1 },
            deactivateAudioSession: { deactivations += 1 }
        )

        playback.play(blob: blob)

        XCTAssertEqual(receivedBlob, blob)
        XCTAssertEqual(receivedHint, AVFileType.m4a.rawValue)
        XCTAssertEqual(activations, 1)
        XCTAssertTrue(fake.prepared)
        XCTAssertTrue(fake.played)
        XCTAssertTrue(playback.isPlaying)
        XCTAssertFalse(playback.playbackFailed)

        fake.onFinish?(true)

        XCTAssertFalse(playback.isPlaying)
        XCTAssertFalse(playback.playbackFailed)
        XCTAssertTrue(fake.stopped)
        XCTAssertEqual(deactivations, 1)
    }

    func testFailedStartIsVisibleAndReleasesAudioSession() {
        let fake = FakeVoiceMemoAudioPlayer()
        fake.canPlay = false
        var deactivations = 0
        let playback = VoiceMemoPlaybackController(
            playerFactory: { _, _ in fake },
            activateAudioSession: {},
            deactivateAudioSession: { deactivations += 1 }
        )

        playback.play(blob: Data([1]))

        XCTAssertFalse(playback.isPlaying)
        XCTAssertTrue(playback.playbackFailed)
        XCTAssertTrue(fake.stopped)
        XCTAssertEqual(deactivations, 1)
    }

    func testToggleStopsActivePlayback() {
        let fake = FakeVoiceMemoAudioPlayer()
        let playback = VoiceMemoPlaybackController(
            playerFactory: { _, _ in fake },
            activateAudioSession: {},
            deactivateAudioSession: {}
        )

        playback.toggle(blob: Data([1]))
        playback.toggle(blob: Data([1]))

        XCTAssertFalse(playback.isPlaying)
        XCTAssertTrue(fake.stopped)
    }

    func testDecoderFailureIsVisibleAfterPlaybackStarts() {
        let fake = FakeVoiceMemoAudioPlayer()
        let playback = VoiceMemoPlaybackController(
            playerFactory: { _, _ in fake },
            activateAudioSession: {},
            deactivateAudioSession: {}
        )

        playback.play(blob: Data([1]))
        fake.onFinish?(false)

        XCTAssertFalse(playback.isPlaying)
        XCTAssertTrue(playback.playbackFailed)
    }
}

private final class FakeVoiceMemoAudioPlayer: VoiceMemoAudioPlaying {
    var onFinish: ((Bool) -> Void)?
    var canPrepare = true
    var canPlay = true
    private(set) var prepared = false
    private(set) var played = false
    private(set) var stopped = false

    func prepareToPlay() -> Bool {
        prepared = true
        return canPrepare
    }

    func play() -> Bool {
        played = true
        return canPlay
    }

    func stop() {
        stopped = true
    }
}
