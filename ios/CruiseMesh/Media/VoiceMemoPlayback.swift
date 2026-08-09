import AVFoundation
import Combine
import Foundation
import OSLog

protocol VoiceMemoAudioPlaying: AnyObject {
    var onFinish: ((Bool) -> Void)? { get set }
    /// Seconds played so far.
    var currentTime: TimeInterval { get set }
    /// Seconds the decoder found in the blob, which is the honest total.
    var duration: TimeInterval { get }
    func prepareToPlay() -> Bool
    func play() -> Bool
    func pause()
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

    var currentTime: TimeInterval {
        get { player.currentTime }
        set { player.currentTime = newValue }
    }

    var duration: TimeInterval { player.duration }

    func prepareToPlay() -> Bool { player.prepareToPlay() }
    func play() -> Bool { player.play() }
    func pause() { player.pause() }
    func stop() { player.stop() }

    func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        onFinish?(flag)
    }

    func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
        onFinish?(false)
    }
}

/// Owns one voice-message playback attempt, including the shared audio session.
///
/// Recorded messages are MPEG-4/AAC. Supplying the M4A type hint is important:
/// `AVAudioPlayer(data:)` does not have a filename extension from which to infer
/// the container, and failed to start for the inline attachment bytes on device.
///
/// The session is `.playback`/`.spokenAudio` and is activated only around a
/// playing message. It deliberately never asks for a communication route: on
/// this project audio does not disturb the mesh, and a hands-free route would
/// put speech on the same radio the mesh runs on.
final class VoiceMemoPlaybackController: ObservableObject {
    typealias PlayerFactory = (Data, String) throws -> VoiceMemoAudioPlaying
    private static let log = Logger(subsystem: "com.cruisemesh", category: "VoiceMessage")

    @Published private(set) var isPlaying = false
    @Published private(set) var playbackFailed = false
    /// Seconds played, for the bubble's progress line.
    @Published private(set) var elapsed: TimeInterval = 0
    /// Seconds the decoder reported, or 0 until a player exists.
    @Published private(set) var total: TimeInterval = 0

    private let playerFactory: PlayerFactory
    private let activateAudioSession: () throws -> Void
    private let deactivateAudioSession: () -> Void
    private var player: VoiceMemoAudioPlaying?
    private var playbackToken: UUID?
    private var ownsAudioSession = false
    private var progressTimer: AnyCancellable?

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

    /// Play, or pause a message that is already playing. A paused message
    /// resumes where it stopped rather than starting over.
    func toggle(blob: Data) {
        if isPlaying {
            pause()
        } else if let player {
            resume(player)
        } else {
            play(blob: blob)
        }
    }

    func play(blob: Data) {
        reset(clearFailure: true)
        do {
            Self.log.info("Preparing voice message playback (\(blob.count, privacy: .public) bytes, M4A)")
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
                    Self.log.error("Voice message decoder stopped with an error")
                }
            }

            guard next.prepareToPlay(), next.play() else {
                throw VoiceMemoPlaybackError.couldNotStart
            }
            total = max(0, next.duration)
            elapsed = 0
            isPlaying = true
            startTicking()
        } catch {
            reset(clearFailure: false)
            playbackFailed = true
            Self.log.error("Could not start voice message playback: \(error.localizedDescription, privacy: .public)")
        }
    }

    func pause() {
        guard let player, isPlaying else { return }
        player.pause()
        isPlaying = false
        elapsed = max(0, player.currentTime)
        progressTimer = nil
    }

    /// Stops and releases the player and the audio session.
    func stop() {
        reset(clearFailure: true)
    }

    private func resume(_ player: VoiceMemoAudioPlaying) {
        guard player.play() else {
            reset(clearFailure: false)
            playbackFailed = true
            return
        }
        isPlaying = true
        startTicking()
    }

    private func startTicking() {
        progressTimer = Timer.publish(every: 0.1, on: .main, in: .common)
            .autoconnect()
            .sink { [weak self] _ in
                guard let self, let player = self.player, self.isPlaying else { return }
                self.elapsed = max(0, player.currentTime)
            }
    }

    private func reset(clearFailure: Bool) {
        progressTimer = nil
        playbackToken = nil
        player?.onFinish = nil
        player?.stop()
        player = nil
        isPlaying = false
        elapsed = 0
        total = 0
        if clearFailure {
            playbackFailed = false
        }
        if ownsAudioSession {
            ownsAudioSession = false
            deactivateAudioSession()
        }
    }

    deinit {
        progressTimer = nil
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
