import Foundation

/// Pure seek/progress arithmetic for a voice-message bubble.
///
/// Mirrors Android's `VoicePlaybackDisplay`: the bar may show the sender's
/// stated duration before the decoder has spoken, but that number is never a
/// seek target. Kept free of AVFoundation so it unit-tests directly.
enum VoicePlaybackDisplay {
    /// Map a 0...1 bar position onto milliseconds of decoder time.
    /// `nil` when the decoder has not reported a positive length, or when
    /// `fraction` is not a real number.
    static func seekTargetMs(decoderDurationMs: Int?, fraction: Double) -> Int? {
        guard let duration = decoderDurationMs, duration > 0, fraction.isFinite else {
            return nil
        }
        let clamped = min(max(fraction, 0), 1)
        return min(max(Int(clamped * Double(duration)), 0), duration)
    }

    static func progressFraction(positionMs: Int, totalMs: Int) -> Double {
        guard totalMs > 0 else { return 0 }
        return min(max(Double(positionMs) / Double(totalMs), 0), 1)
    }
}
