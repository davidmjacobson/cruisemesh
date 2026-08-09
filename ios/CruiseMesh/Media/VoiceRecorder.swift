import AVFoundation
import Foundation
import OSLog

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
final class VoiceRecorder: NSObject {
    static var plan: CoreVoiceCapturePlan { voiceCapturePlan() }
    static var maxDurationSeconds: TimeInterval { TimeInterval(plan.maxDurationMs) / 1000 }

    private static let log = Logger(subsystem: "com.cruisemesh", category: "VoiceMessage")

    private var recorder: AVAudioRecorder?
    private var outputURL: URL?

    var isRecording: Bool { recorder?.isRecording == true }

    func start() -> Bool {
        cancel()
        let plan = Self.plan
        let session = AVAudioSession.sharedInstance()
        do {
            // Deliberately *not* `.allowBluetooth`. That option routes capture
            // to a headset's hands-free profile, which shares the radio the
            // mesh runs on; on this project audio never disturbs the mesh (see
            // BluetoothAudioBackoff). Recording stays on the built-in mic and
            // the session is deactivated the moment it ends.
            try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker])
            try session.setActive(true)
        } catch {
            Self.log.error("Could not activate the recording session: \(error.localizedDescription, privacy: .public)")
            deactivateAudioSession()
            return false
        }

        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("voice", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("memo-\(Int(Date().timeIntervalSince1970 * 1000)).m4a")
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
            AVSampleRateKey: Double(plan.sampleRateHz),
            AVNumberOfChannelsKey: 1,
            AVEncoderBitRateKey: Int(plan.bitrateBps),
        ]
        do {
            let rec = try AVAudioRecorder(url: url, settings: settings)
            guard rec.prepareToRecord() else {
                Self.log.error("Could not prepare the M4A voice recorder")
                try? FileManager.default.removeItem(at: url)
                deactivateAudioSession()
                return false
            }
            // Backstop only: the composer's gesture state machine stops at the
            // plan's bound. This covers a UI that somehow stopped ticking.
            guard rec.record(forDuration: Self.maxDurationSeconds + Self.maxDurationBackstopSeconds) else {
                Self.log.error("Could not start the M4A voice recorder")
                try? FileManager.default.removeItem(at: url)
                deactivateAudioSession()
                return false
            }
            recorder = rec
            outputURL = url
            return true
        } catch {
            Self.log.error("Could not create the M4A voice recorder: \(error.localizedDescription, privacy: .public)")
            try? FileManager.default.removeItem(at: url)
            deactivateAudioSession()
            return false
        }
    }

    /// Stops and returns (file URL, duration ms), or nil.
    func stop() -> (URL, Int32)? {
        guard let recorder, let url = outputURL else {
            cancel()
            return nil
        }
        let bound = Double(Self.plan.maxDurationMs)
        let duration = Int32(min(recorder.currentTime * 1_000, bound))
        recorder.stop()
        deactivateAudioSession()
        self.recorder = nil
        self.outputURL = nil
        guard FileManager.default.fileExists(atPath: url.path),
              let bytes = try? Data(contentsOf: url),
              !bytes.isEmpty else {
            Self.log.error("Voice recording stopped without a readable M4A file")
            try? FileManager.default.removeItem(at: url)
            return nil
        }
        Self.log.info(
            "Finished voice recording (\(bytes.count, privacy: .public) bytes, \(duration, privacy: .public) ms)"
        )
        return (url, max(0, duration))
    }

    func cancel() {
        let hadActiveRecording = recorder != nil || outputURL != nil
        recorder?.stop()
        if hadActiveRecording {
            deactivateAudioSession()
        }
        recorder = nil
        if let url = outputURL {
            try? FileManager.default.removeItem(at: url)
        }
        outputURL = nil
    }

    private static let maxDurationBackstopSeconds: TimeInterval = 5

    private func deactivateAudioSession() {
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
    }
}
