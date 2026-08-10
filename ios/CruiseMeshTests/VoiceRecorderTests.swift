import AVFoundation
import XCTest
@testable import CruiseMesh

/// These cover the decidable pieces of the recorder: the settings dict, the
/// "never send an unplayable memo" gate, and the finish flow through an injected
/// capture double. Real microphone capture cannot run on CI, so none of these
/// touch audio hardware — they exercise the logic that decides whether a file is
/// safe to send.
final class VoiceRecorderTests: XCTestCase {
    // MARK: Settings dict

    func testRecorderSettingsPinPortableAacContainer() {
        let plan = VoiceRecorder.plan
        let settings = VoiceRecorder.recorderSettings(plan: plan)

        XCTAssertEqual(settings[AVFormatIDKey] as? Int, Int(kAudioFormatMPEG4AAC))
        XCTAssertEqual(settings[AVSampleRateKey] as? Double, Double(plan.sampleRateHz))
        XCTAssertEqual(settings[AVNumberOfChannelsKey] as? Int, 1)
        XCTAssertEqual(settings[AVEncoderBitRateKey] as? Int, Int(plan.bitrateBps))
        XCTAssertEqual(settings[AVEncoderAudioQualityKey] as? Int, AVAudioQuality.medium.rawValue)
    }

    // MARK: The pure "safe to send?" decision

    func testFinalizeAcceptsAPlayableFile() {
        let decision = VoiceRecorder.decideFinalized(
            .init(success: true, byteCount: 4_096, decodedDurationMs: 2_500),
            requestedDurationMs: 2_400
        )
        XCTAssertEqual(decision, .accept(durationMs: 2_400))
    }

    func testFinalizeRejectsWhenRecorderReportedFailure() {
        let decision = VoiceRecorder.decideFinalized(
            .init(success: false, byteCount: 4_096, decodedDurationMs: 2_500),
            requestedDurationMs: 2_400
        )
        XCTAssertEqual(decision, .reject(reason: .recorderReportedFailure))
    }

    func testFinalizeRejectsEmptyOrTinyFile() {
        let decision = VoiceRecorder.decideFinalized(
            .init(success: true, byteCount: 10, decodedDurationMs: 2_500),
            requestedDurationMs: 2_400
        )
        XCTAssertEqual(decision, .reject(reason: .fileMissingOrEmpty))
    }

    /// The classic un-finalized MPEG-4: bytes on disk (samples) but no `moov`, so
    /// the decoder finds no duration. This is the file the field iPad produced.
    func testFinalizeRejectsFileThatDoesNotDecode() {
        let missing = VoiceRecorder.decideFinalized(
            .init(success: true, byteCount: 4_096, decodedDurationMs: nil),
            requestedDurationMs: 2_400
        )
        XCTAssertEqual(missing, .reject(reason: .didNotDecode))

        let zero = VoiceRecorder.decideFinalized(
            .init(success: true, byteCount: 4_096, decodedDurationMs: 0),
            requestedDurationMs: 2_400
        )
        XCTAssertEqual(zero, .reject(reason: .didNotDecode))
    }

    // MARK: The finish flow through an injected capture

    func testStartFailsWhenTheRecorderWillNotRecord() {
        let recorder = VoiceRecorder(
            captureFactory: { url, _ in
                let fake = FakeVoiceCapture(url: url)
                fake.canRecord = false
                return fake
            },
            activateSession: { Self.fakeSession },
            deactivateSession: {},
            decodeDurationMs: { _ in 1_000 }
        )
        XCTAssertFalse(recorder.start())
    }

    func testStopDeliversFileWhenFinalizeSucceeds() {
        var deactivations = 0
        let recorder = VoiceRecorder(
            captureFactory: { url, _ in
                let fake = FakeVoiceCapture(url: url)
                fake.finishSuccess = true
                fake.bytesToWrite = 4_096
                return fake
            },
            activateSession: { Self.fakeSession },
            deactivateSession: { deactivations += 1 },
            decodeDurationMs: { _ in 1_800 },
            tailDrainSeconds: 0
        )

        XCTAssertTrue(recorder.start())

        var result: (URL, Int32)??
        recorder.stop { result = $0 }

        XCTAssertNotNil(result ?? nil, "a playable recording must be delivered")
        XCTAssertEqual((result ?? nil)?.1, 0, "duration is the clamped hold, ~0ms in a synchronous test")
        XCTAssertGreaterThanOrEqual(deactivations, 1, "the session is handed back after a send")
        if let url = (result ?? nil)?.0 { try? FileManager.default.removeItem(at: url) }
    }

    /// A recording the recorder reported as failed must never be sent.
    func testStopAbortsWhenDelegateReportsFailure() {
        let recorder = VoiceRecorder(
            captureFactory: { url, _ in
                let fake = FakeVoiceCapture(url: url)
                fake.finishSuccess = false
                fake.bytesToWrite = 4_096
                return fake
            },
            activateSession: { Self.fakeSession },
            deactivateSession: {},
            decodeDurationMs: { _ in 1_800 },
            tailDrainSeconds: 0
        )

        XCTAssertTrue(recorder.start())

        var called = false
        var result: (URL, Int32)?
        recorder.stop {
            called = true
            result = $0
        }

        XCTAssertTrue(called)
        XCTAssertNil(result, "a failed recording must abort the send, not attach a dead file")
    }

    /// A file that does not decode (no `moov`) must never be sent, even if the
    /// delegate reported success.
    func testStopAbortsWhenFileDoesNotDecode() {
        let recorder = VoiceRecorder(
            captureFactory: { url, _ in
                let fake = FakeVoiceCapture(url: url)
                fake.finishSuccess = true
                fake.bytesToWrite = 4_096
                return fake
            },
            activateSession: { Self.fakeSession },
            deactivateSession: {},
            decodeDurationMs: { _ in nil },
            tailDrainSeconds: 0
        )

        XCTAssertTrue(recorder.start())

        var result: (URL, Int32)?
        recorder.stop { result = $0 }

        XCTAssertNil(result, "an undecodable file must abort the send")
    }

    // MARK: The tail-drain decision and the drained stop

    func testTailDrainWindowKeepsRecorderRunningTailWhileRecording() {
        // A normal release, still recording: keep the pipeline running for the
        // configured window so the buffered tail is written before teardown.
        XCTAssertEqual(VoiceRecorder.drainWindowSeconds(stillRecording: true, configured: 0.5), 0.5)
    }

    func testTailDrainWindowIsZeroAfterAHardBackstop() {
        // The recorder already stopped itself at its max-duration backstop: it is
        // finalized and stop() is a no-op, so there is nothing to drain.
        XCTAssertEqual(VoiceRecorder.drainWindowSeconds(stillRecording: false, configured: 0.5), 0)
    }

    func testTailDrainWindowClampsNegativeConfiguration() {
        XCTAssertEqual(VoiceRecorder.drainWindowSeconds(stillRecording: true, configured: -1), 0)
    }

    func testDefaultTailDrainMatchesTheFieldMeasuredLoss() {
        XCTAssertGreaterThanOrEqual(VoiceRecorder.defaultTailDrainSeconds, 0.4)
        XCTAssertLessThanOrEqual(VoiceRecorder.defaultTailDrainSeconds, 0.6)
    }

    /// With a positive drain the file is delivered only *after* the drain window,
    /// and the finalize gate still runs — the recording is not dropped by the
    /// delay.
    func testStopWithDrainStillDeliversAfterTheWindow() {
        let recorder = VoiceRecorder(
            captureFactory: { url, _ in
                let fake = FakeVoiceCapture(url: url)
                fake.finishSuccess = true
                fake.bytesToWrite = 4_096
                return fake
            },
            activateSession: { Self.fakeSession },
            deactivateSession: {},
            decodeDurationMs: { _ in 1_800 },
            tailDrainSeconds: 0.2
        )

        XCTAssertTrue(recorder.start())

        var result: (URL, Int32)??
        recorder.stop { result = $0 }

        // The drain is scheduled on the main queue, so nothing is delivered yet.
        XCTAssertNil(result ?? nil, "delivery must wait for the drain window")

        let delivered = expectation(description: "recording delivered after drain")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
            if result != nil { delivered.fulfill() }
        }
        wait(for: [delivered], timeout: 2.0)
        XCTAssertNotNil(result ?? nil, "a playable recording must be delivered after the drain")
        if let url = (result ?? nil)?.0 { try? FileManager.default.removeItem(at: url) }
    }

    private static let fakeSession = VoiceRecorder.SessionInfo(
        category: "AVAudioSessionCategoryPlayAndRecord",
        mode: "AVAudioSessionModeDefault",
        hardwareSampleRate: 48_000
    )
}

private final class FakeVoiceCapture: VoiceCapturing {
    let url: URL
    var onFinish: ((Bool) -> Void)?
    var canPrepare = true
    var canRecord = true
    var finishSuccess = true
    var bytesToWrite = 2_048
    private(set) var isRecording = false

    init(url: URL) { self.url = url }

    var fileSizeBytes: UInt64 {
        let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
        return (attributes?[.size] as? NSNumber)?.uint64Value ?? 0
    }

    func prepareToRecord() -> Bool { canPrepare }

    func record(forDuration _: TimeInterval) -> Bool {
        isRecording = canRecord
        return canRecord
    }

    func stop() {
        isRecording = false
        if bytesToWrite > 0 {
            try? Data(count: bytesToWrite).write(to: url)
        }
        onFinish?(finishSuccess)
    }
}
