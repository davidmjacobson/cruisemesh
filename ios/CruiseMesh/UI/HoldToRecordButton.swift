import AVFoundation
import Combine
import SwiftUI
import UIKit

/// The composer's push-to-talk control.
///
/// The gesture's meaning — what counts as a cancel slide, what counts as a lock
/// slide, how short a press is a mis-tap, when a recording has run long enough
/// or grown large enough to stop itself — lives in `core/src/voice.rs` and is
/// shared with Android. This view only turns touches into those events and
/// renders the answer.
struct HoldToRecordButton: View {
    let recorder: VoiceRecorder
    /// True while a recording is running, so the composer can keep this control
    /// mounted and take the text field away. Losing this view mid-recording
    /// would take `.onDisappear` with it and silently discard what was said.
    @Binding var isCapturing: Bool
    let onFinished: (URL, Int32) -> Void
    let onError: (String) -> Void
    let onAccessibilityFallback: () -> Void

    @Environment(\.scenePhase) private var scenePhase

    @State private var capture = voiceCaptureIdleState()
    @State private var startedAt: TimeInterval = 0
    @State private var elapsed: TimeInterval = 0
    /// True from the first `onChanged` of a drag until its `onEnded`.
    ///
    /// A recording that ends on its own — the duration bound, the byte budget —
    /// puts the phase back to `.idle` while the finger is still down. Without
    /// this latch the very next `onChanged` (a millimetre of jitter is enough)
    /// reads `.idle`, takes the press branch, and starts a *second* recording
    /// under the same uninterrupted hold, which arrives as a second voice
    /// message the user never asked to send.
    @State private var gestureInProgress = false

    private var isRecording: Bool { capture.phase != .idle }

    /// Posted when a call, an alarm, or another app takes the microphone away.
    private var interruptions: NotificationCenter.Publisher {
        NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification)
    }

    // The body is deliberately split into small, separately type-checked
    // pieces. A single chain of five modifiers, each with its own closure over
    // this view's state, is more than the Swift type checker will solve here —
    // it gives up with "failed to produce diagnostic", which names no line
    // worth reading.
    var body: some View {
        watchingLifecycle(ticking(control))
    }

    @ViewBuilder
    private var control: some View {
        if capture.phase == .locked {
            handsFreeControls
        } else {
            holdControl
        }
    }

    /// Only ticks while a recording is running: the id goes back to `.idle`
    /// when it stops, and the task returns immediately.
    private func ticking<V: View>(_ content: V) -> some View {
        content.task(id: capture.phase) { await tick() }
    }

    private func watchingLifecycle<V: View>(_ content: V) -> some View {
        content
            .onChange(of: capture.phase) { (phase: VoiceCapturePhase) in
                isCapturing = phase != .idle
            }
            .onChange(of: scenePhase) { (phase: ScenePhase) in
                stopIfBackgrounded(phase)
            }
            .onReceive(interruptions) { (note: Notification) in
                handleInterruption(note)
            }
            .onDisappear {
                cancelOnDisappear()
            }
    }

    /// A hands-free recording outlives the finger, so it can outlive the
    /// screen. iOS has no `audio` background mode here (and should not grow one
    /// for this), so the session is torn down under the running recorder the
    /// moment the app backgrounds. Stop and say so rather than let a locked
    /// recording auto-send whatever survived.
    private func stopIfBackgrounded(_ phase: ScenePhase) {
        guard phase == .background, isRecording else { return }
        cancelRecording(because: String(localized: "Recording stopped when you left the app"))
    }

    private func cancelOnDisappear() {
        guard isRecording else { return }
        capture = voiceCaptureIdleState()
        isCapturing = false
        recorder.cancel()
    }

    private func tick() async {
        while !Task.isCancelled, capture.phase != .idle {
            try? await Task.sleep(nanoseconds: 100_000_000)
            guard capture.phase != .idle else { return }
            elapsed = monotonicNow() - startedAt
            apply(voiceCaptureElapsed(state: capture, elapsedMs: elapsedMs()))
            guard capture.phase != .idle else { return }
            // The clock is not the only bound. An encoder that clamps the
            // bitrate we asked for upward fills the envelope early, and the
            // user finding that out after they have spoken is exactly the
            // failure this is here to prevent.
            apply(voiceCaptureBytes(state: capture, bytesWritten: recorder.bytesRecorded()))
        }
    }

    private func handleInterruption(_ note: Notification) {
        guard isRecording else { return }
        guard let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt else { return }
        guard let type = AVAudioSession.InterruptionType(rawValue: raw), type == .began else { return }
        cancelRecording(
            because: String(localized: "Recording stopped when something else needed the microphone")
        )
    }

    private var holdControl: some View {
        HStack(spacing: 8) {
            Image(systemName: isRecording ? "waveform" : "mic.fill")
                .foregroundStyle(iconTint)
            if isRecording {
                Text(elapsedLabel)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(capture.cancelArmed ? .red : .secondary)
                    .lineLimit(2)
                    .minimumScaleFactor(0.8)
                Spacer(minLength: 0)
            }
        }
        .frame(height: 36)
        // While recording the composer hands this control the whole row (the
        // text field is gone), which is the only way the slide hint fits.
        .frame(maxWidth: recordingRowWidth)
        .padding(.horizontal, isRecording ? 10 : 2)
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { value in
                    let wasAlreadyDown = gestureInProgress
                    gestureInProgress = true
                    if capture.phase == .idle {
                        // The finger never came up since the last recording
                        // ended: this drag is spent, not a new press.
                        guard !wasAlreadyDown else { return }
                        let pressed = voiceCapturePress(state: capture)
                        guard pressed.effect == .start, recorder.start() else {
                            onError(String(localized: "Microphone unavailable"))
                            return
                        }
                        startedAt = monotonicNow()
                        elapsed = 0
                        withAnimation(.easeOut(duration: 0.15)) { capture = pressed.state }
                        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                        return
                    }
                    guard capture.phase == .holding else { return }
                    capture = voiceCaptureDrag(
                        state: capture,
                        dx: Float(value.translation.width),
                        dy: Float(value.translation.height)
                    ).state
                }
                .onEnded { _ in
                    gestureInProgress = false
                    guard capture.phase == .holding else { return }
                    apply(voiceCaptureRelease(state: capture, elapsedMs: elapsedMs()))
                }
        )
        .accessibilityLabel("Hold to talk")
        .accessibilityAction(named: "Record with controls") {
            onAccessibilityFallback()
        }
    }

    /// After a slide-up lock the finger is off the button, so the same slot
    /// becomes an ordinary pair of controls.
    private var handsFreeControls: some View {
        HStack(spacing: 10) {
            Image(systemName: "waveform")
                .foregroundStyle(.red)
            Text(elapsedLabel)
                .font(.caption.monospacedDigit())
            Spacer(minLength: 0)
            Button("Cancel") {
                apply(voiceCaptureCancel(state: capture))
            }
            .font(.caption)
            Button {
                apply(voiceCaptureFinish(state: capture, elapsedMs: elapsedMs()))
            } label: {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 28, weight: .semibold))
            }
            .accessibilityLabel("Send voice message")
        }
        .frame(height: 36)
        .frame(maxWidth: .infinity)
    }

    /// Full width while recording, natural width otherwise.
    private var recordingRowWidth: CGFloat? {
        isRecording ? .infinity : nil
    }

    private var iconTint: Color {
        guard isRecording else { return .secondary }
        return capture.cancelArmed ? .gray : .red
    }

    /// What releasing right now would do — and, before either threshold is
    /// armed, what the two slides are for. A timer alone would leave both
    /// gestures undiscoverable.
    private var hint: String {
        if capture.cancelArmed { return String(localized: "Release to cancel") }
        if capture.lockArmed { return String(localized: "Release for hands-free") }
        return String(localized: "Slide left to cancel, up for hands-free")
    }

    private var elapsedLabel: String {
        let seconds = Int(elapsed)
        // Not String(format: "%d:%02d", ...): Int is 64-bit here and %d reads
        // 32, which is the kind of thing that works until it does not.
        return "\(seconds / 60):" + String(format: "%02d", seconds % 60)
    }

    /// Monotonic, not wall time: a carrier or NTP correction landing mid-hold
    /// must not shorten or lengthen what the user recorded. Ships are exactly
    /// where phones re-sync their clocks.
    private func monotonicNow() -> TimeInterval {
        ProcessInfo.processInfo.systemUptime
    }

    private func elapsedMs() -> UInt32 {
        let millis = (monotonicNow() - startedAt) * 1000
        return UInt32(max(0, min(millis, Double(UInt32.max))))
    }

    private func cancelRecording(because reason: String) {
        gestureInProgress = false
        apply(voiceCaptureCancel(state: capture))
        onError(reason)
    }

    private func apply(_ step: CoreVoiceCaptureStep) {
        let wasRecording = isRecording
        withAnimation(.easeOut(duration: 0.15)) { capture = step.state }
        switch step.effect {
        case .send:
            if let result = recorder.stop() {
                onFinished(result.0, result.1)
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
            } else {
                onError(String(localized: "Could not save that voice message"))
            }
        case .discardTooShort:
            recorder.cancel()
            if wasRecording { onError(String(localized: "Hold the mic to talk")) }
        case .discardCancelled:
            recorder.cancel()
        case .start, VoiceCaptureEffect.none:
            break
        }
    }
}
