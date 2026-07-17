import AVFoundation
import Foundation

final class VoiceRecorder: NSObject {
    static let maxDurationSeconds: TimeInterval = 60

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
            rec.prepareToRecord()
            guard rec.record(forDuration: Self.maxDurationSeconds) else {
                deactivateAudioSession()
                return false
            }
            recorder = rec
            outputURL = url
            return true
        } catch {
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
              (try? Data(contentsOf: url))?.isEmpty == false else {
            try? FileManager.default.removeItem(at: url)
            return nil
        }
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
