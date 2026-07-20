import SwiftUI
import UIKit

/// Pure swipe-to-reply math (T1), mirroring Android's `SwipeToReplyLogic` so the
/// two platforms feel identical. Kept free of SwiftUI so it can be unit-tested.
enum SwipeToReplyMath {
    /// Past `maxDrag` the bubble keeps moving but at a fraction of the finger.
    static let rubberBand: CGFloat = 0.15

    /// Offset the bubble should show for a raw rightward drag of `rawDrag` px.
    /// Leftward drags are ignored; past `maxDrag` the offset rubber-bands.
    static func clampOffset(_ rawDrag: CGFloat, maxDrag: CGFloat) -> CGFloat {
        if rawDrag <= 0 { return 0 }
        if rawDrag <= maxDrag { return rawDrag }
        return maxDrag + (rawDrag - maxDrag) * rubberBand
    }

    /// Whether releasing at `offset` should start a reply.
    static func shouldReply(offset: CGFloat, threshold: CGFloat) -> Bool {
        offset >= threshold
    }

    /// Fraction 0...1 of the way to the trigger threshold, for icon fade/scale.
    static func progress(offset: CGFloat, threshold: CGFloat) -> CGFloat {
        guard threshold > 0 else { return 0 }
        return min(max(offset / threshold, 0), 1)
    }
}

/// Signal-style swipe-to-reply: a rightward drag translates the bubble and
/// reveals a reply arrow; releasing past the threshold starts a reply. The drag
/// only engages for horizontal-dominant movement so the conversation still
/// scrolls vertically.
private struct SwipeToReplyModifier: ViewModifier {
    let onReply: () -> Void

    @State private var offset: CGFloat = 0
    @State private var triggered = false

    private let threshold: CGFloat = 56
    private let maxDrag: CGFloat = 80

    func body(content: Content) -> some View {
        let progress = SwipeToReplyMath.progress(offset: offset, threshold: threshold)
        return ZStack(alignment: .leading) {
            Image(systemName: "arrowshape.turn.up.left.fill")
                .foregroundStyle(.tint)
                .padding(.leading, 20)
                .opacity(Double(progress))
                .scaleEffect(0.7 + 0.3 * progress)
            content
                .offset(x: offset)
                .gesture(
                    DragGesture(minimumDistance: 15)
                        .onChanged { value in
                            guard abs(value.translation.width) > abs(value.translation.height) else { return }
                            offset = SwipeToReplyMath.clampOffset(value.translation.width, maxDrag: maxDrag)
                            if !triggered, SwipeToReplyMath.shouldReply(offset: offset, threshold: threshold) {
                                triggered = true
                                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                            }
                        }
                        .onEnded { _ in
                            if SwipeToReplyMath.shouldReply(offset: offset, threshold: threshold) {
                                onReply()
                            }
                            triggered = false
                            withAnimation(.spring(response: 0.3, dampingFraction: 0.7)) { offset = 0 }
                        }
                )
        }
    }
}

extension View {
    /// Attach swipe-to-reply (T1) to a message bubble.
    func swipeToReply(onReply: @escaping () -> Void) -> some View {
        modifier(SwipeToReplyModifier(onReply: onReply))
    }
}
