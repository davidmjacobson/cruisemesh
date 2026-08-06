import AVFoundation
import Foundation
import OSLog

final class VoiceRecorder: NSObject {
    static let maxDurationSeconds: TimeInterval = 60
    private static let log = Logger(subsystem: "com.cruisemesh", category: "VoiceMemo")

    private var recorder: AVAudioRecorder?
    private var outputURL: URL?

    var isRecording: Bool { recorder?.isRecording == true }

    func start() -> Bool {
        cancel()
        let session = AVAudioSession.sharedInstance()
        do {
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
            AVSampleRateKey: 16_000,
            AVNumberOfChannelsKey: 1,
            AVEncoderBitRateKey: 32_000,
        ]
        do {
            let rec = try AVAudioRecorder(url: url, settings: settings)
            guard rec.prepareToRecord() else {
                Self.log.error("Could not prepare the M4A voice recorder")
                try? FileManager.default.removeItem(at: url)
                deactivateAudioSession()
                return false
            }
            guard rec.record(forDuration: Self.maxDurationSeconds) else {
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
        let duration = Int32(min(recorder.currentTime * 1_000, Double(Int32.max)))
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

    private func deactivateAudioSession() {
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
    }
}
