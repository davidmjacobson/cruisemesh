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

/// What one voice bubble needs to draw itself.
struct VoiceBubbleState: Equatable {
    let isPlaying: Bool
    /// Seconds played so far.
    let elapsed: TimeInterval
    /// The decoder's length once it has spoken, otherwise the duration the
    /// sender stated. Display only — never a seek target.
    let total: TimeInterval
    let failed: Bool
}

/// Owns voice-message playback for a whole conversation, above the message list.
///
/// It lives above the list on purpose. Playback used to be owned by the bubble
/// itself, which meant a message stopped the moment its bubble left the screen:
/// a `LazyVStack` disposes a row that scrolls out of view, and the bubble's
/// `.onDisappear` stopped the player. New messages arriving mid-listen scroll
/// the thread on their own, so a minute-long message in a live conversation was
/// close to impossible to hear to the end. Held here, playback survives
/// scrolling and the bubble re-attaches to it when it comes back; it stops only
/// when the listener leaves the conversation or stops it themselves.
///
/// A message is identified by a stable key (its sender and lamport, by way of
/// the conversation's row id) rather than by view identity, matching Android's
/// `VoiceMessagePlayback`. One controller per conversation also makes "one
/// message at a time" structural: starting a second message stops the first,
/// and the shared audio session keeps that true across conversations.
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

    /// The message loaded in the decoder, playing or paused. Nil when nothing
    /// is loaded, including once a message has played to its end.
    @Published private(set) var activeKey: String?
    @Published private(set) var isPlaying = false
    /// The message whose last attempt failed, if any. One at a time: a failure
    /// is about the attempt, and starting another message is a new attempt.
    @Published private(set) var failedKey: String?
    /// Seconds played, for the bubble's progress line.
    @Published private(set) var elapsed: TimeInterval = 0
    /// Seconds the decoder reported, or 0 until a player exists.
    @Published private(set) var total: TimeInterval = 0

    /// The controller currently holding the shared audio session, if any.
    ///
    /// Each conversation owns a controller, but the process has exactly one
    /// `AVAudioSession`. Without an owner of record, starting a message in a
    /// second conversation while a first one plays leaves both playing on top
    /// of each other, and pausing either one deactivates the session out from
    /// under the other. Taking the session stops whoever had it, so at most one
    /// voice message is ever audible and `ownsAudioSession` is a true claim
    /// rather than a hopeful one.
    ///
    /// Weak: a conversation that goes away deallocates its controller, and a
    /// dead controller must not keep the session reserved.
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

    /// What the bubble for `key` should draw. Every message that is not the
    /// active one is idle at 0:00 over the duration its sender stated.
    func state(for key: String, manifestDurationMs: Int32) -> VoiceBubbleState {
        let stated = TimeInterval(max(0, manifestDurationMs)) / 1_000
        guard key == activeKey else {
            return VoiceBubbleState(
                isPlaying: false,
                elapsed: 0,
                total: stated,
                failed: key == failedKey
            )
        }
        return VoiceBubbleState(
            isPlaying: isPlaying,
            elapsed: elapsed,
            total: total > 0 ? total : stated,
            failed: key == failedKey
        )
    }

    /// Jump `key` to `fraction` of the decoder's duration. A no-op until the
    /// decoder has reported a length, and a no-op for any other message.
    /// Playing stays playing; paused stays paused.
    func seek(key: String, fraction: Double) {
        guard let player, key == activeKey else { return }
        let decoderMs = Int((total * 1000).rounded(.down))
        guard let targetMs = VoicePlaybackDisplay.seekTargetMs(
            decoderDurationMs: decoderMs,
            fraction: fraction
        ) else { return }
        let target = TimeInterval(targetMs) / 1_000
        player.currentTime = target
        elapsed = target
    }

    /// Play `key`, pause it if it is already playing, or resume it where it
    /// stopped. Starting a different message stops whatever was playing.
    func toggle(key: String, blob: Data) {
        guard key == activeKey else {
            play(key: key, blob: blob)
            return
        }
        if isPlaying {
            pause()
        } else if let player {
            resume(player, key: key)
        } else {
            play(key: key, blob: blob)
        }
    }

    func play(key: String, blob: Data) {
        reset(clearFailure: true)
        do {
            Self.log.info("Preparing voice message playback (\(blob.count, privacy: .public) bytes, M4A)")
            let next = try playerFactory(blob, AVFileType.m4a.rawValue)
            try takeAudioSession()

            let token = UUID()
            playbackToken = token
            player = next
            activeKey = key
            next.onFinish = { [weak self] succeeded in
                guard let self, self.playbackToken == token else { return }
                self.reset(clearFailure: succeeded)
                if !succeeded {
                    self.failedKey = key
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
            reset(clearFailure: true)
            failedKey = key
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

    /// Stops and releases the player and the audio session. Leaving the
    /// conversation, or an explicit stop — not scrolling.
    func stop() {
        reset(clearFailure: true)
    }

    private func resume(_ player: VoiceMemoAudioPlaying, key: String) {
        do {
            try takeAudioSession()
        } catch {
            reset(clearFailure: true)
            failedKey = key
            Self.log.error("Could not reactivate the session to resume: \(error.localizedDescription, privacy: .public)")
            return
        }
        guard player.play() else {
            reset(clearFailure: true)
            failedKey = key
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
        activeKey = nil
        isPlaying = false
        elapsed = 0
        total = 0
        if clearFailure {
            failedKey = nil
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
