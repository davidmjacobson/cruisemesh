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
        XCTAssertTrue(fake.paused)
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

    /// The bubble shows elapsed over total, so the total has to be the
    /// decoder's, not the duration the sender claimed.
    func testTotalComesFromTheDecoderAndElapsedTracksPlayback() {
        let fake = FakeVoiceMemoAudioPlayer()
        fake.duration = 12.5
        var factoryCalls = 0
        let playback = VoiceMemoPlaybackController(
            playerFactory: { _, _ in
                factoryCalls += 1
                return fake
            },
            activateAudioSession: {},
            deactivateAudioSession: {}
        )

        playback.play(blob: Data([1]))

        XCTAssertEqual(playback.total, 12.5)
        XCTAssertEqual(playback.elapsed, 0)

        // Pausing captures where the message got to, and resuming does not
        // start it over.
        fake.currentTime = 4
        playback.toggle(blob: Data([1]))

        XCTAssertFalse(playback.isPlaying)
        XCTAssertEqual(playback.elapsed, 4)
        XCTAssertEqual(factoryCalls, 1)

        playback.toggle(blob: Data([1]))

        XCTAssertTrue(playback.isPlaying)
        XCTAssertEqual(factoryCalls, 1, "resuming must not decode the blob a second time")
        XCTAssertEqual(fake.currentTime, 4, "resuming must not rewind")
    }

    func testStoppingClearsProgress() {
        let fake = FakeVoiceMemoAudioPlayer()
        fake.duration = 8
        let playback = VoiceMemoPlaybackController(
            playerFactory: { _, _ in fake },
            activateAudioSession: {},
            deactivateAudioSession: {}
        )

        playback.play(blob: Data([1]))
        playback.stop()

        XCTAssertEqual(playback.elapsed, 0)
        XCTAssertEqual(playback.total, 0)
        XCTAssertFalse(playback.isPlaying)
    }
}

private final class FakeVoiceMemoAudioPlayer: VoiceMemoAudioPlaying {
    var onFinish: ((Bool) -> Void)?
    var canPrepare = true
    var canPlay = true
    var currentTime: TimeInterval = 0
    var duration: TimeInterval = 0
    private(set) var prepared = false
    private(set) var played = false
    private(set) var paused = false
    private(set) var stopped = false

    func prepareToPlay() -> Bool {
        prepared = true
        return canPrepare
    }

    func play() -> Bool {
        played = true
        return canPlay
    }

    func pause() {
        paused = true
    }

    func stop() {
        stopped = true
    }
}
