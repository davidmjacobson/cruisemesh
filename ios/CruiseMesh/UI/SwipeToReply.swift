import SwiftUI
import UIKit

/// Pure swipe-to-reply math (T1), mirroring Android's `SwipeToReplyLogic` so the
/// two platforms feel identical. Kept free of SwiftUI so it can be unit-tested.
enum SwipeToReplyMath {
    /// Past `maxDrag` the bubble keeps moving but at a fraction of the finger.
    static let rubberBand: CGFloat = 0.15

    /// How far a finger travels before it counts as a swipe at all.
    static let engageDistance: CGFloat = 15

    /// Whether a drag of `translation` should move the bubble, given whether the
    /// bubble is already moving.
    ///
    /// A bubble only takes over a drag that starts out sideways-dominant; the
    /// thread owns every other drag, so an up/down swipe that happens to land on
    /// a bubble still scrolls and still dismisses the keyboard. Once a swipe has
    /// taken over, it keeps the drag even if the finger curves, so a reply in
    /// progress doesn't snap back mid-gesture.
    static func engages(
        translation: CGSize,
        alreadyEngaged: Bool,
        scrubbing: Bool = false
    ) -> Bool {
        // A voice-message seek bar lives inside the bubble. Its drag is
        // simultaneous with this swipe, so a rightward scrub past the
        // reply threshold would also start a reply unless we opt out.
        if scrubbing { return false }
        guard translation.width > 0 else { return false }
        if alreadyEngaged { return true }
        return abs(translation.width) > abs(translation.height)
    }

    /// Offset the bubble should show for a raw rightward drag of `rawDrag` px.
    /// Leftward drags are ignored; past `maxDrag` the offset rubber-bands.
    static func clampOffset(_ rawDrag: CGFloat, maxDrag: CGFloat) -> CGFloat {
        if rawDrag <= 0 { return 0 }
        if rawDrag <= maxDrag { return rawDrag }
        return maxDrag + (rawDrag - maxDrag) * rubberBand
    }

    /// Whether releasing at `offset` should start a reply.
    static func shouldReply(
        offset: CGFloat,
        threshold: CGFloat,
        scrubbing: Bool = false
    ) -> Bool {
        if scrubbing { return false }
        return offset >= threshold
    }

    /// Fraction 0...1 of the way to the trigger threshold, for icon fade/scale.
    static func progress(offset: CGFloat, threshold: CGFloat) -> CGFloat {
        guard threshold > 0 else { return 0 }
        return min(max(offset / threshold, 0), 1)
    }
}

/// Synchronous handshake with `VoiceMemoSeekBar`. Preference updates land
/// a frame late, so the first 15 pt of a rightward scrub can already have
/// engaged swipe-to-reply; this flag is set in the seek bar's `onChanged`.
enum VoiceSeekDrag {
    private static let lock = NSLock()
    private static var count = 0

    static var isActive: Bool {
        lock.lock()
        defer { lock.unlock() }
        return count > 0
    }

    static func begin() {
        lock.lock()
        count += 1
        lock.unlock()
    }

    static func end() {
        lock.lock()
        count = max(0, count - 1)
        lock.unlock()
    }
}

/// Signal-style swipe-to-reply: a rightward drag translates the bubble and
/// reveals a reply arrow; releasing past the threshold starts a reply. The drag
/// recognises alongside the thread's own scrolling and only engages for
/// horizontal-dominant movement, so a drag that starts on a bubble still
/// scrolls the conversation.
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
                // Simultaneous, not `.gesture`: an exclusive drag gesture on a
                // bubble beats the enclosing thread's scroll gesture, so every
                // bubble became a dead zone -- a tall photo worst of all --
                // where the thread would not scroll and dragging down would not
                // dismiss the keyboard. Recognising alongside the scroll keeps
                // the thread moving; the direction check below keeps the bubble
                // still while it does.
                .simultaneousGesture(
                    DragGesture(minimumDistance: SwipeToReplyMath.engageDistance)
                        .onChanged { value in
                            let scrubbing = VoiceSeekDrag.isActive
                            guard SwipeToReplyMath.engages(
                                translation: value.translation,
                                alreadyEngaged: offset > 0,
                                scrubbing: scrubbing
                            ) else {
                                // The thread owns this drag, or a seek bar
                                // does. Reset rather than ignore: scrolling
                                // cancels our gesture, so `onEnded` may never
                                // arrive to spring a part-swiped bubble back.
                                if offset != 0 { offset = 0 }
                                if triggered { triggered = false }
                                return
                            }
                            offset = SwipeToReplyMath.clampOffset(value.translation.width, maxDrag: maxDrag)
                            if !triggered, SwipeToReplyMath.shouldReply(
                                offset: offset,
                                threshold: threshold,
                                scrubbing: scrubbing
                            ) {
                                triggered = true
                                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                            }
                        }
                        .onEnded { _ in
                            if SwipeToReplyMath.shouldReply(
                                offset: offset,
                                threshold: threshold,
                                scrubbing: VoiceSeekDrag.isActive
                            ) {
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
