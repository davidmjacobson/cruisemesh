import AVFoundation
import Combine
import Foundation
import OSLog

protocol VoiceMemoAudioPlaying: AnyObject {
    var onFinish: ((Bool) -> Void)? { get set }
    func prepareToPlay() -> Bool
    func play() -> Bool
    func stop()
}

final class SystemVoiceMemoAudioPlayer: NSObject, VoiceMemoAudioPlaying, AVAudioPlayerDelegate {
    var onFinish: ((Bool) -> Void)?

    private let player: AVAudioPlayer

    init(data: Data, fileTypeHint: String) throws {
        player = try AVAudioPlayer(data: data, fileTypeHint: fileTypeHint)
        super.init()
        player.delegate = self
    }

    func prepareToPlay() -> Bool { player.prepareToPlay() }
    func play() -> Bool { player.play() }
    func stop() { player.stop() }

    func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        onFinish?(flag)
    }

    func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
        onFinish?(false)
    }
}

/// Owns one voice-memo playback attempt, including the shared audio session.
///
/// Recorded memos are MPEG-4/AAC. Supplying the M4A type hint is important:
/// `AVAudioPlayer(data:)` does not have a filename extension from which to infer
/// the container, and failed to start for the inline attachment bytes on device.
final class VoiceMemoPlaybackController: ObservableObject {
    typealias PlayerFactory = (Data, String) throws -> VoiceMemoAudioPlaying
    private static let log = Logger(subsystem: "com.cruisemesh", category: "VoiceMemo")

    @Published private(set) var isPlaying = false
    @Published private(set) var playbackFailed = false

    private let playerFactory: PlayerFactory
    private let activateAudioSession: () throws -> Void
    private let deactivateAudioSession: () -> Void
    private var player: VoiceMemoAudioPlaying?
    private var playbackToken: UUID?
    private var ownsAudioSession = false

    init(
        playerFactory: @escaping PlayerFactory = { data, fileTypeHint in
            try SystemVoiceMemoAudioPlayer(data: data, fileTypeHint: fileTypeHint)
        },
        activateAudioSession: @escaping () throws -> Void = {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, mode: .spokenAudio)
            try session.setActive(true)
        },
        deactivateAudioSession: @escaping () -> Void = {
            try? AVAudioSession.sharedInstance().setActive(
                false,
                options: [.notifyOthersOnDeactivation]
            )
        }
    ) {
        self.playerFactory = playerFactory
        self.activateAudioSession = activateAudioSession
        self.deactivateAudioSession = deactivateAudioSession
    }

    func toggle(blob: Data) {
        if isPlaying {
            stop()
        } else {
            play(blob: blob)
        }
    }

    func play(blob: Data) {
        reset(clearFailure: true)
        do {
            Self.log.info("Preparing voice memo playback (\(blob.count, privacy: .public) bytes, M4A)")
            let next = try playerFactory(blob, AVFileType.m4a.rawValue)
            ownsAudioSession = true
            try activateAudioSession()

            let token = UUID()
            playbackToken = token
            player = next
            next.onFinish = { [weak self] succeeded in
                guard self?.playbackToken == token else { return }
                self?.reset(clearFailure: succeeded)
                self?.playbackFailed = !succeeded
                if !succeeded {
                    Self.log.error("Voice memo decoder stopped with an error")
                }
            }

            guard next.prepareToPlay(), next.play() else {
                throw VoiceMemoPlaybackError.couldNotStart
            }
            isPlaying = true
        } catch {
            reset(clearFailure: false)
            playbackFailed = true
            Self.log.error("Could not start voice memo playback: \(error.localizedDescription, privacy: .public)")
        }
    }

    func stop() {
        reset(clearFailure: true)
    }

    private func reset(clearFailure: Bool) {
        playbackToken = nil
        player?.onFinish = nil
        player?.stop()
        player = nil
        isPlaying = false
        if clearFailure {
            playbackFailed = false
        }
        if ownsAudioSession {
            ownsAudioSession = false
            deactivateAudioSession()
        }
    }

    deinit {
        player?.onFinish = nil
        player?.stop()
        if ownsAudioSession {
            deactivateAudioSession()
        }
    }
}

private enum VoiceMemoPlaybackError: Error {
    case couldNotStart
}
