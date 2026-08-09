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

    /// The controller currently holding the shared audio session, if any.
    ///
    /// Every voice bubble in the timeline owns its own controller, but the
    /// process has exactly one `AVAudioSession`. Without an owner of record,
    /// tapping a second message while a first one plays leaves both playing on
    /// top of each other, and pausing either one deactivates the session out
    /// from under the other. Taking the session stops whoever had it, so at most
    /// one voice message is ever audible and `ownsAudioSession` is a true claim
    /// rather than a hopeful one.
    ///
    /// Weak: a bubble that scrolls away deallocates, and a dead controller must
    /// not keep the session reserved.
    private static weak var sessionHolder: VoiceMemoPlaybackController?

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
            try takeAudioSession()

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

    /// Pauses and hands the audio session back.
    ///
    /// Holding a `.playback` session open would keep every other app's audio
    /// ducked for as long as the message sits paused, which could be until the
    /// user leaves the chat. The player itself is kept so resuming picks up
    /// where it stopped instead of decoding the blob again.
    func pause() {
        guard let player, isPlaying else { return }
        player.pause()
        isPlaying = false
        elapsed = max(0, player.currentTime)
        progressTimer = nil
        releaseAudioSession()
    }

    /// Stops and releases the player and the audio session.
    func stop() {
        reset(clearFailure: true)
    }

    private func resume(_ player: VoiceMemoAudioPlaying) {
        do {
            try takeAudioSession()
        } catch {
            reset(clearFailure: false)
            playbackFailed = true
            Self.log.error("Could not reactivate the session to resume: \(error.localizedDescription, privacy: .public)")
            return
        }
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
        releaseAudioSession()
    }

    /// Claims the shared audio session for this message, stopping whichever
    /// message had it. See [`sessionHolder`].
    private func takeAudioSession() throws {
        if let holder = Self.sessionHolder, holder !== self {
            holder.stop()
        }
        // Claimed before activating, so a throw still leaves the failure path's
        // `releaseAudioSession()` able to hand back a half-taken session.
        ownsAudioSession = true
        Self.sessionHolder = self
        try activateAudioSession()
    }

    private func releaseAudioSession() {
        guard ownsAudioSession else { return }
        ownsAudioSession = false
        if Self.sessionHolder === self {
            Self.sessionHolder = nil
        }
        deactivateAudioSession()
    }

    deinit {
        progressTimer = nil
        player?.onFinish = nil
        player?.stop()
        // No need to clear `sessionHolder` here: it is weak, so it has already
        // zeroed itself by the time this runs.
        if ownsAudioSession {
            deactivateAudioSession()
        }
    }
}

private enum VoiceMemoPlaybackError: Error {
    case couldNotStart
}
