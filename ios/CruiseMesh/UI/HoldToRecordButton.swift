import SwiftUI
import UIKit

struct HoldToRecordButton: View {
    let recorder: VoiceRecorder
    let onFinished: (URL, Int32) -> Void
    let onError: (String) -> Void
    let onAccessibilityFallback: () -> Void

    @State private var pressing = false
    @State private var cancelPending = false
    @State private var startedAt = Date.distantPast

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: pressing ? "waveform" : "mic.fill")
                .foregroundStyle(pressing ? .red : .secondary)
            if pressing {
                Text(cancelPending ? "Release to cancel" : "Slide left to cancel")
                    .font(.caption)
                    .foregroundStyle(cancelPending ? .red : .secondary)
                    .lineLimit(1)
            }
        }
        .frame(height: 36)
        .padding(.horizontal, pressing ? 10 : 2)
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { value in
                    if !pressing {
                        guard recorder.start() else {
                            onError("Microphone unavailable")
                            return
                        }
                        startedAt = Date()
                        withAnimation(.easeOut(duration: 0.15)) { pressing = true }
                        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                    }
                    cancelPending = value.translation.width < -70
                }
                .onEnded { _ in
                    guard pressing else { return }
                    let held = Date().timeIntervalSince(startedAt)
                    if cancelPending || held < 0.5 {
                        recorder.cancel()
                        if held < 0.5 && !cancelPending {
                            onError("Hold the mic to record a voice memo")
                        }
                    } else if let result = recorder.stop() {
                        onFinished(result.0, result.1)
                        UIImpactFeedbackGenerator(style: .light).impactOccurred()
                    } else {
                        onError("Could not save voice memo")
                    }
                    withAnimation(.easeOut(duration: 0.15)) { pressing = false }
                    cancelPending = false
                }
        )
        .accessibilityLabel("Hold to record a voice memo")
        .accessibilityAction(named: "Record with controls") {
            onAccessibilityFallback()
        }
    }
}
