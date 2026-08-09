import SwiftUI
import UIKit

/// The composer's push-to-talk control.
///
/// The gesture's meaning — what counts as a cancel slide, what counts as a lock
/// slide, how short a press is a mis-tap, when a recording has run long enough
/// to stop itself — lives in `core/src/voice.rs` and is shared with Android.
/// This view only turns touches into those events and renders the answer.
struct HoldToRecordButton: View {
    let recorder: VoiceRecorder
    let onFinished: (URL, Int32) -> Void
    let onError: (String) -> Void
    let onAccessibilityFallback: () -> Void

    @State private var capture = voiceCaptureIdleState()
    @State private var startedAt = Date.distantPast
    @State private var elapsed: TimeInterval = 0

    private var isRecording: Bool { capture.phase != .idle }

    var body: some View {
        Group {
            if capture.phase == .locked {
                handsFreeControls
            } else {
                holdControl
            }
        }
        // Only ticks while a recording is running: the id goes back to `.idle`
        // when it stops, and the task returns immediately.
        .task(id: capture.phase) {
            while !Task.isCancelled, capture.phase != .idle {
                try? await Task.sleep(nanoseconds: 100_000_000)
                guard capture.phase != .idle else { return }
                elapsed = Date().timeIntervalSince(startedAt)
                apply(voiceCaptureElapsed(state: capture, elapsedMs: elapsedMs()))
            }
        }
        .onDisappear {
            guard isRecording else { return }
            capture = voiceCaptureIdleState()
            recorder.cancel()
        }
    }

    private var holdControl: some View {
        HStack(spacing: 8) {
            Image(systemName: isRecording ? "waveform" : "mic.fill")
                .foregroundStyle(iconTint)
            if isRecording {
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(capture.cancelArmed ? .red : .secondary)
                    .lineLimit(1)
            }
        }
        .frame(height: 36)
        .padding(.horizontal, isRecording ? 10 : 2)
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { value in
                    if capture.phase == .idle {
                        let pressed = voiceCapturePress(state: capture)
                        guard pressed.effect == .start, recorder.start() else {
                            onError("Microphone unavailable")
                            return
                        }
                        startedAt = Date()
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
    }

    private var iconTint: Color {
        guard isRecording else { return .secondary }
        return capture.cancelArmed ? .gray : .red
    }

    private var hint: String {
        if capture.cancelArmed { return String(localized: "Release to cancel") }
        if capture.lockArmed { return String(localized: "Release for hands-free") }
        return elapsedLabel
    }

    private var elapsedLabel: String {
        let seconds = Int(elapsed)
        return String(format: "%d:%02d", seconds / 60, seconds % 60)
    }

    private func elapsedMs() -> UInt32 {
        let millis = Date().timeIntervalSince(startedAt) * 1000
        return UInt32(max(0, min(millis, Double(UInt32.max))))
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
                onError("Could not save that voice message")
            }
        case .discardTooShort:
            recorder.cancel()
            if wasRecording { onError("Hold the mic to talk") }
        case .discardCancelled:
            recorder.cancel()
        case .start, VoiceCaptureEffect.none:
            break
        }
    }
}
