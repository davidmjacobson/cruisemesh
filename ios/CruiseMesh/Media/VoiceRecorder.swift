import AVFoundation
import Foundation
import OSLog

/// The recorder's dependency on the platform capture object, narrowed to what
/// this file uses so a test can stand in a double that fails on demand (the same
/// seam `VoiceMemoPlayback` uses for `AVAudioPlayer`). The real implementation
/// is `SystemVoiceCapture`, which wraps `AVAudioRecorder` and forwards its
/// finish delegate to `onFinish`.
protocol VoiceCapturing: AnyObject {
    /// Called once when the file has been finalized — the encoder has flushed and
    /// the MPEG-4 `moov` atom has been written — with whether that succeeded.
    ///
    /// This is the whole point of the seam: an MPEG-4 file is only playable after
    /// finalization, and finalization completes *after* `stop()` returns. Reading
    /// the bytes before this fires yields a file with samples but no `moov` — a
    /// file the recording device itself cannot play, which is exactly the field
    /// symptom this replaces.
    var onFinish: ((Bool) -> Void)? { get set }
    var isRecording: Bool { get }
    /// Bytes written to the file so far, for the composer's live byte budget.
    var fileSizeBytes: UInt64 { get }
    func prepareToRecord() -> Bool
    func record(forDuration duration: TimeInterval) -> Bool
    func stop()
}

/// `AVAudioRecorder` behind ``VoiceCapturing``. Owns the recorder and is its
/// delegate so the finish callback can be turned into a plain closure.
final class SystemVoiceCapture: NSObject, VoiceCapturing, AVAudioRecorderDelegate {
    var onFinish: ((Bool) -> Void)?

    private let recorder: AVAudioRecorder
    private let url: URL

    init(url: URL, settings: [String: Any]) throws {
        self.url = url
        recorder = try AVAudioRecorder(url: url, settings: settings)
        super.init()
        recorder.delegate = self
    }

    var isRecording: Bool { recorder.isRecording }

    var fileSizeBytes: UInt64 {
        let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
        return (attributes?[.size] as? NSNumber)?.uint64Value ?? 0
    }

    func prepareToRecord() -> Bool { recorder.prepareToRecord() }
    func record(forDuration duration: TimeInterval) -> Bool { recorder.record(forDuration: duration) }
    func stop() { recorder.stop() }

    func audioRecorderDidFinishRecording(_: AVAudioRecorder, successfully flag: Bool) {
        onFinish?(flag)
    }

    func audioRecorderEncodeErrorDidOccur(_: AVAudioRecorder, error: Error?) {
        // An encode error means the file is not trustworthy even if `stop()` is
        // never reached; report it as a failed finish so the send is aborted.
        onFinish?(false)
    }
}

/// Short push-to-talk voice capture for attachment messages.
///
/// Every number here comes from the core's `voiceCapturePlan()` so iOS and
/// Android cannot drift apart on what fits in one envelope; see
/// `core/src/voice.rs` for the bitrate/duration arithmetic.
///
/// AAC-LC in an MPEG-4 container is the encoding on both platforms.
/// `AVAudioRecorder` can also produce Opus, but only into a CAF container that
/// Android's `MediaPlayer` cannot read, and the Android encoder's Opus output is
/// Ogg, which `AVAudioPlayer` cannot read — so AAC is the one encoding both
/// shells can both write and play.
final class VoiceRecorder {
    static var plan: CoreVoiceCapturePlan { voiceCapturePlan() }
    static var maxDurationSeconds: TimeInterval { TimeInterval(plan.maxDurationMs) / 1000 }

    private static let log = Logger(subsystem: "com.cruisemesh", category: "VoiceMessage")

    /// A finalized MPEG-4/AAC file smaller than this is a header with no usable
    /// audio; treat it as a failed recording. The real gate is that the file
    /// decodes (``decodeDurationMs``); this only rejects the obviously-empty
    /// case early and cheaply.
    static let minValidBytes = 512

    // MARK: Injectable seams (defaults are the real platform, tests supply doubles)

    typealias CaptureFactory = (URL, [String: Any]) throws -> VoiceCapturing
    /// Info about the session that was actually activated, for the diagnostics
    /// line — the owner reads this from the flight recorder to confirm the fix.
    struct SessionInfo {
        var category: String
        var mode: String
        var hardwareSampleRate: Double
    }
    typealias SessionActivator = () throws -> SessionInfo
    typealias DurationProbe = (URL) -> Int32?

    private let makeCapture: CaptureFactory
    private let activateSession: SessionActivator
    private let deactivateSession: () -> Void
    private let decodeDurationMs: DurationProbe
    /// How long to keep the recorder running after the user releases, before
    /// `stop()` tears the capture pipeline down — the tail-drain (see
    /// ``drainWindowSeconds(stillRecording:configured:)``). Injectable so tests
    /// can pin it to 0 and stay synchronous.
    private let tailDrainSeconds: TimeInterval

    private var capture: VoiceCapturing?
    private var outputURL: URL?
    /// Set while a stop is finalizing; the file the finish/timeout will read.
    /// Held apart from `outputURL` so a `cancel()` racing an in-flight finalize
    /// cannot delete the bytes out from under the send.
    private var finalizingURL: URL?
    private var pendingFinish: (((URL, Int32)?) -> Void)?
    private var sessionInfo: SessionInfo?
    private var lastSettings: [String: Any] = [:]

    /// Monotonic start stamp.
    ///
    /// Not `AVAudioRecorder.currentTime`: that reads 0 the moment the recorder
    /// is no longer recording, so an interrupted or backstopped recording would
    /// be stamped 0 ms and the bubble would say "0:00 / 0:00" over a minute of
    /// speech. Not `Date()` either — a clock correction mid-hold (time-zone
    /// change at sea, cell reacquisition in port) must not change what the user
    /// recorded.
    private var startedAt: TimeInterval = 0

    init(
        captureFactory: @escaping CaptureFactory = { url, settings in
            try SystemVoiceCapture(url: url, settings: settings)
        },
        activateSession: @escaping SessionActivator = VoiceRecorder.activateSharedSession,
        deactivateSession: @escaping () -> Void = VoiceRecorder.deactivateSharedSession,
        decodeDurationMs: @escaping DurationProbe = VoiceRecorder.decodeDurationMs(url:),
        tailDrainSeconds: TimeInterval = VoiceRecorder.defaultTailDrainSeconds
    ) {
        self.makeCapture = captureFactory
        self.activateSession = activateSession
        self.deactivateSession = deactivateSession
        self.decodeDurationMs = decodeDurationMs
        self.tailDrainSeconds = tailDrainSeconds
    }

    var isRecording: Bool { capture?.isRecording == true }

    /// Bytes the encoder has written so far, for the composer's byte-budget
    /// check. Zero when nothing is recording.
    ///
    /// This is an in-progress MPEG-4 file, so it understates the finished size
    /// by the sample tables still to be written at stop; the core's byte budget
    /// holds a container allowance back for exactly that.
    func bytesRecorded() -> UInt32 {
        guard let capture else { return 0 }
        return UInt32(min(capture.fileSizeBytes, UInt64(UInt32.max)))
    }

    func start() -> Bool {
        cancel()
        let plan = Self.plan
        let session: SessionInfo
        do {
            session = try activateSession()
        } catch {
            Self.log.error(
                "Could not activate the recording session: \(error.localizedDescription, privacy: .public)"
            )
            deactivateSession()
            return false
        }

        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("voice", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("memo-\(Int(Date().timeIntervalSince1970 * 1000)).m4a")
        let settings = Self.recorderSettings(plan: plan)
        do {
            let cap = try makeCapture(url, settings)
            guard cap.prepareToRecord() else {
                Self.log.error("Could not prepare the M4A voice recorder")
                try? FileManager.default.removeItem(at: url)
                deactivateSession()
                return false
            }
            // Backstop only: the composer's gesture state machine stops at the
            // plan's bound. This covers a UI that somehow stopped ticking.
            guard cap.record(forDuration: Self.maxDurationSeconds + Self.maxDurationBackstopSeconds) else {
                Self.log.error("Could not start the M4A voice recorder")
                try? FileManager.default.removeItem(at: url)
                deactivateSession()
                return false
            }
            capture = cap
            outputURL = url
            sessionInfo = session
            lastSettings = settings
            startedAt = ProcessInfo.processInfo.systemUptime
            return true
        } catch {
            Self.log.error("Could not create the M4A voice recorder: \(error.localizedDescription, privacy: .public)")
            try? FileManager.default.removeItem(at: url)
            deactivateSession()
            return false
        }
    }

    /// The recorder configuration, pinned to the core's portable-format contract
    /// (AAC-LC in MPEG-4 at a widely-decodable rate). Pure and static so a test
    /// can assert the dict without touching audio hardware.
    static func recorderSettings(plan: CoreVoiceCapturePlan) -> [String: Any] {
        [
            AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
            AVSampleRateKey: Double(plan.sampleRateHz),
            AVNumberOfChannelsKey: 1,
            AVEncoderBitRateKey: Int(plan.bitrateBps),
            // AAC-LC specifically (not HE-AAC): the profile Android's MediaPlayer
            // decodes most reliably across versions.
            AVEncoderAudioQualityKey: AVAudioQuality.medium.rawValue,
        ]
    }

    /// Stops and delivers (file URL, duration ms), or nil if the recording could
    /// not be finalized into a playable file.
    ///
    /// Asynchronous by necessity: the file is only playable once the encoder has
    /// flushed and the `moov` atom has been written, which happens after
    /// `stop()` returns and is signalled by the finish delegate. The completion
    /// runs on the main queue.
    func stop(completion: @escaping ((URL, Int32)?) -> Void) {
        guard let capture, let url = outputURL else {
            cancel()
            completion(nil)
            return
        }
        let requested = clampedDurationMs()
        // Move the file out of `outputURL` so a `cancel()` (e.g. the recorder
        // sheet's onDismiss) cannot delete it while we finalize.
        outputURL = nil
        finalizingURL = url
        pendingFinish = completion

        capture.onFinish = { [weak self] success in
            self?.finish(success: success, requestedDurationMs: requested)
        }

        // Backstop: if the finish delegate never fires we still finalize, and the
        // decode gate below will reject a file that never got its `moov`.
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.finalizeTimeoutSeconds) { [weak self] in
            guard let self, self.pendingFinish != nil else { return }
            Self.log.error("Voice recording finalize timed out; validating the file as written")
            self.finish(success: true, requestedDurationMs: requested)
        }

        // Tail drain: keep the recorder capturing for a short window before
        // stopping so the pipeline's in-flight tail — the ~0.4-0.5 s that an
        // immediate stop() discards when the user lifts their finger on the last
        // word — is written to the file before teardown. This is a delay *before*
        // stop(); everything above (the finish delegate, the finalize timeout,
        // the "only read the file after onFinish" gate from #297) is unchanged —
        // the file is never read during the drain, only after finalize. A
        // recorder that already hit its own hard backstop is no longer recording
        // and is finalized, so it is stopped immediately with no drain.
        let drain = Self.drainWindowSeconds(stillRecording: capture.isRecording, configured: tailDrainSeconds)
        Self.log.info("Voice stop: draining \(drain, privacy: .public)s before finalize")
        if drain > 0 {
            DispatchQueue.main.asyncAfter(deadline: .now() + drain) { capture.stop() }
        } else {
            capture.stop()
        }
    }

    /// Default post-release drain window, matched to the Android side
    /// (`VoiceDrainPlan.DRAIN_WINDOW_MS`): the field-measured tail loss was
    /// ~0.4-0.5 s and the capture/encoder pipeline latency that causes it is not
    /// queryable, so a fixed window is the honest bound. The cost is a little
    /// trailing ambient audio, which beats clipping the last spoken word.
    static let defaultTailDrainSeconds: TimeInterval = 0.5

    /// The drain window to actually use, pure and static so it is unit-tested
    /// without audio hardware.
    ///
    /// - Parameter stillRecording: whether the capture is still running. A
    ///   recorder that already stopped itself at its max-duration backstop is
    ///   finalized; there is nothing to drain and `stop()` would be a no-op, so
    ///   the window collapses to 0.
    /// - Parameter configured: the requested window (tests pin this to 0).
    static func drainWindowSeconds(stillRecording: Bool, configured: TimeInterval) -> TimeInterval {
        guard stillRecording else { return 0 }
        return max(0, configured)
    }

    /// Runs the finalize decision and delivers (or rejects) the recording.
    ///
    /// Threading invariant: this must only be called on the main thread, so the
    /// two callers — the finalize timeout (scheduled on `DispatchQueue.main`)
    /// and the capture's `onFinish` delegate — never race the shared state
    /// (`pendingFinish`, `finalizingURL`, `capture`) and no lock is needed.
    /// `AVAudioRecorderDelegate` callbacks are documented to be delivered on the
    /// thread that started recording, which here is the main thread; the
    /// `pendingFinish` nil-out at the top is the one-shot guard that makes
    /// whichever caller arrives first win and the other a no-op.
    private func finish(success: Bool, requestedDurationMs requested: Int32) {
        guard let completion = pendingFinish else { return }
        pendingFinish = nil
        let url = finalizingURL
        finalizingURL = nil
        let cap = capture
        capture = nil
        cap?.onFinish = nil
        startedAt = 0

        deactivateSession()

        guard let url else {
            deliver(nil, completion)
            return
        }

        let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
        let byteCount = Int((attributes?[.size] as? NSNumber)?.uint64Value ?? 0)
        let decoded = decodeDurationMs(url)
        let decision = Self.decideFinalized(
            .init(success: success, byteCount: byteCount, decodedDurationMs: decoded),
            requestedDurationMs: requested
        )
        logFinalize(success: success, byteCount: byteCount, decoded: decoded, decision: decision)

        switch decision {
        case let .accept(durationMs):
            deliver((url, durationMs), completion)
        case .reject:
            try? FileManager.default.removeItem(at: url)
            deliver(nil, completion)
        }
    }

    /// What was captured about a finalized file, gathered by ``finish`` and
    /// judged by ``decideFinalized``.
    struct Finalized {
        var success: Bool
        var byteCount: Int
        var decodedDurationMs: Int32?
    }

    enum FinalizeDecision: Equatable {
        case accept(durationMs: Int32)
        case reject(reason: RejectReason)
    }

    enum RejectReason: String {
        /// The finish delegate (or an encode error) reported failure.
        case recorderReportedFailure
        /// The finalized file is missing or too small to hold audio.
        case fileMissingOrEmpty
        /// The file exists but does not decode — the classic un-finalized MPEG-4
        /// (samples written, no `moov`) that plays nowhere, including here.
        case didNotDecode
    }

    /// The whole "never send an unplayable memo" decision, pure and static so it
    /// is unit-tested directly without audio hardware.
    static func decideFinalized(_ f: Finalized, requestedDurationMs requested: Int32) -> FinalizeDecision {
        guard f.success else { return .reject(reason: .recorderReportedFailure) }
        guard f.byteCount >= minValidBytes else { return .reject(reason: .fileMissingOrEmpty) }
        guard let decoded = f.decodedDurationMs, decoded > 0 else { return .reject(reason: .didNotDecode) }
        return .accept(durationMs: max(0, requested))
    }

    func cancel() {
        let hadActiveRecording = capture != nil || outputURL != nil
        capture?.onFinish = nil
        capture?.stop()
        if hadActiveRecording {
            deactivateSession()
        }
        capture = nil
        if let url = outputURL {
            try? FileManager.default.removeItem(at: url)
        }
        outputURL = nil
        startedAt = 0
        // Note: a stop() already in flight keeps `finalizingURL`/`pendingFinish`
        // and finishes on its own; cancel deliberately does not touch them so a
        // send that has begun still completes (or is rejected) rather than being
        // silently dropped.
    }

    /// Slack between the composer's own stop and the recorder's hard stop.
    /// Every path that drives this recorder stops at the plan's bound on its
    /// own clock; this only covers a ticker that somehow stopped ticking.
    static let maxDurationBackstopSeconds: TimeInterval = 5

    /// How long to wait for the finish delegate before finalizing from disk.
    /// Finalization is normally tens of milliseconds; this only bounds a
    /// delegate that never arrives so the UI cannot hang.
    ///
    /// There is a narrow false-negative window: if the delegate is merely very
    /// late (a thermally throttled iPad whose finalize genuinely exceeds this
    /// bound), the timeout runs `finish(success: true)`, the still-un-finalized
    /// file fails the decode gate, is deleted, and the send is aborted with a
    /// "try again". That is the safe failure — we would rather ask for a retry
    /// than send a dead memo — and it is generously bounded here so only a
    /// pathological stall reaches it for a short PTT recording.
    static let finalizeTimeoutSeconds: TimeInterval = 8

    private func clampedDurationMs() -> Int32 {
        let bound = Double(Self.plan.maxDurationMs)
        let held = (ProcessInfo.processInfo.systemUptime - startedAt) * 1_000
        return Int32(max(0, min(held, bound)))
    }

    private func deliver(_ result: (URL, Int32)?, _ completion: @escaping ((URL, Int32)?) -> Void) {
        if Thread.isMainThread {
            completion(result)
        } else {
            DispatchQueue.main.async { completion(result) }
        }
    }

    private func logFinalize(
        success: Bool,
        byteCount: Int,
        decoded: Int32?,
        decision: FinalizeDecision
    ) {
        let session = sessionInfo
        let category = session?.category ?? "unknown"
        let mode = session?.mode ?? "unknown"
        let hwRate = session?.hardwareSampleRate ?? 0
        let askedRate = (lastSettings[AVSampleRateKey] as? Double) ?? 0
        let askedBitrate = (lastSettings[AVEncoderBitRateKey] as? Int) ?? 0
        let decodedText = decoded.map { String($0) } ?? "nil"
        switch decision {
        case let .accept(durationMs):
            Self.log.info(
                """
                Voice recording finalized OK — session category=\(category, privacy: .public) \
                mode=\(mode, privacy: .public) hwRate=\(hwRate, privacy: .public) \
                settings rate=\(askedRate, privacy: .public) bitrate=\(askedBitrate, privacy: .public) \
                delegateSuccess=\(success, privacy: .public) bytes=\(byteCount, privacy: .public) \
                decodedMs=\(decodedText, privacy: .public) durationMs=\(durationMs, privacy: .public)
                """
            )
        case let .reject(reason):
            Self.log.error(
                """
                Voice recording REJECTED (\(reason.rawValue, privacy: .public)) — session \
                category=\(category, privacy: .public) mode=\(mode, privacy: .public) \
                hwRate=\(hwRate, privacy: .public) settings rate=\(askedRate, privacy: .public) \
                bitrate=\(askedBitrate, privacy: .public) delegateSuccess=\(success, privacy: .public) \
                bytes=\(byteCount, privacy: .public) decodedMs=\(decodedText, privacy: .public)
                """
            )
        }
    }

    // MARK: Real platform implementations of the seams

    /// Activates the shared session for recording and reports what it became, so
    /// a failure is surfaced (return value / throw) rather than swallowed and the
    /// diagnostics line can name the route the iPad actually gave us.
    static func activateSharedSession() throws -> SessionInfo {
        let session = AVAudioSession.sharedInstance()
        // Deliberately *not* `.allowBluetooth`. That option routes capture to a
        // headset's hands-free profile, which shares the radio the mesh runs on;
        // on this project audio never disturbs the mesh (see
        // BluetoothAudioBackoff). Recording stays on the built-in mic and the
        // session is deactivated the moment it ends.
        try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker])
        try session.setActive(true)
        return SessionInfo(
            category: session.category.rawValue,
            mode: session.mode.rawValue,
            hardwareSampleRate: session.sampleRate
        )
    }

    static func deactivateSharedSession() {
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
    }

    /// The finalized-file gate that matters: open the bytes with the same decoder
    /// playback uses and read the duration. A file that does not open here — the
    /// un-finalized MPEG-4 with no `moov` — is a file the recording device itself
    /// cannot play, so it must never be sent.
    static func decodeDurationMs(url: URL) -> Int32? {
        guard let player = try? AVAudioPlayer(contentsOf: url) else { return nil }
        let seconds = player.duration
        guard seconds.isFinite, seconds > 0 else { return nil }
        return Int32(min(seconds * 1_000, Double(Int32.max)))
    }
}
