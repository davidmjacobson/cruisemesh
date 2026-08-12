import CoreGraphics
import Foundation

/// Drawing model and geometry for the pre-send photo markup editor
/// (`specs/photo-markup.md`). The Android half is
/// `chat/PhotoMarkupModel.kt`; this is the same feature, same defaults, same
/// rules, written twice.
///
/// Deliberately free of SwiftUI and UIKit so every rule here -- what undo
/// removes, how a finger position on screen becomes a pixel in the photo, how
/// wide a "thick" stroke is, whether an annotated photo still fits -- is a
/// plain XCTest rather than an on-device experiment. `PhotoMarkupEditor` holds
/// one of these and does nothing but render it.

/// A point in whichever space the caller is working in: view points while a
/// finger is down, image pixels once `MarkupFit.toImagePoint` has mapped it.
struct MarkupPoint: Equatable {
    let x: CGFloat
    let y: CGFloat

    init(x: CGFloat, y: CGFloat) {
        self.x = x
        self.y = y
    }

    init(_ point: CGPoint) {
        self.init(x: point.x, y: point.y)
    }

    var cgPoint: CGPoint {
        CGPoint(x: x, y: y)
    }
}

/// Opaque sRGB components for a pen color. A named struct rather than a tuple
/// so a test can say which channel it means.
struct MarkupRGBA: Equatable {
    let red: Double
    let green: Double
    let blue: Double
    let alpha: Double
}

/// The four pen colors. Picked to stay readable on both a washed-out sunlit
/// deck photo and a dark interior one. Red is first because circling a spot is
/// what people actually reach for this to do. Values match Android's swatches
/// channel for channel.
enum MarkupColor: String, CaseIterable {
    case red
    case yellow
    case white
    case black

    static let defaultColor: MarkupColor = .red

    var components: MarkupRGBA {
        switch self {
        case .red:
            return MarkupRGBA(red: 229.0 / 255.0, green: 57.0 / 255.0, blue: 53.0 / 255.0, alpha: 1)
        case .yellow:
            return MarkupRGBA(red: 255.0 / 255.0, green: 212.0 / 255.0, blue: 0.0 / 255.0, alpha: 1)
        case .white:
            return MarkupRGBA(red: 1, green: 1, blue: 1, alpha: 1)
        case .black:
            return MarkupRGBA(red: 17.0 / 255.0, green: 17.0 / 255.0, blue: 17.0 / 255.0, alpha: 1)
        }
    }
}

/// Pen width as a fraction of the photo's longest edge, so the same step looks
/// the same on a wide deck plan and a tall screenshot, and so a stroke drawn on
/// a small phone lands at the right weight in the full-resolution composite.
enum MarkupThickness: String, CaseIterable {
    case thin
    case medium
    case thick

    static let defaultThickness: MarkupThickness = .medium

    var edgeFraction: CGFloat {
        switch self {
        case .thin:
            return 0.006
        case .medium:
            return 0.013
        case .thick:
            return 0.026
        }
    }

    /// Stroke width in pixels for an image whose longest edge is `longestEdge`.
    func width(longestEdge: Int) -> CGFloat {
        max(1, edgeFraction * CGFloat(max(1, longestEdge)))
    }
}

/// One freehand stroke: a color, a width step, and the path the finger took.
struct MarkupStroke: Equatable {
    let color: MarkupColor
    let thickness: MarkupThickness
    var points: [MarkupPoint]
}

/// How a photo of `imageSize` sits inside a viewport, letterboxed and centered
/// (the editor shows the whole photo, never a crop). `scale` is image pixels ->
/// view points; `offsetX`/`offsetY` are the letterbox margins.
struct MarkupFit: Equatable {
    let scale: CGFloat
    let offsetX: CGFloat
    let offsetY: CGFloat

    /// Identity fit, used before the editor has been measured. Named `identity`
    /// rather than `none` so it can never be confused with `Optional.none` at a
    /// call site that expects a `MarkupFit?`.
    static let identity: MarkupFit = MarkupFit(scale: 1, offsetX: 0, offsetY: 0)

    /// Maps a touch in view coordinates back to a pixel in the decoded image.
    func toImagePoint(_ view: MarkupPoint) -> MarkupPoint {
        MarkupPoint(x: (view.x - offsetX) / scale, y: (view.y - offsetY) / scale)
    }

    /// Maps an image pixel forward to where it is drawn on screen.
    func toViewPoint(_ image: MarkupPoint) -> MarkupPoint {
        MarkupPoint(x: image.x * scale + offsetX, y: image.y * scale + offsetY)
    }

    /// Where the photo itself lands in the viewport. Strokes are clipped to
    /// this, so a finger that slides off the edge of a letterboxed image does
    /// not paint on the black surround.
    func photoRect(imageSize: CGSize) -> CGRect {
        CGRect(
            x: offsetX,
            y: offsetY,
            width: imageSize.width * scale,
            height: imageSize.height * scale
        )
    }
}

/// Computes the letterboxed fit of an image inside a viewport. Returns
/// `MarkupFit.identity` when anything is degenerate, so an unmeasured frame
/// maps points through unchanged instead of dividing by zero.
func markupFit(imageSize: CGSize, viewSize: CGSize) -> MarkupFit {
    guard imageSize.width > 0, imageSize.height > 0,
          viewSize.width > 0, viewSize.height > 0 else {
        return MarkupFit.identity
    }
    let scale: CGFloat = min(
        viewSize.width / imageSize.width,
        viewSize.height / imageSize.height
    )
    return MarkupFit(
        scale: scale,
        offsetX: (viewSize.width - imageSize.width * scale) / 2,
        offsetY: (viewSize.height - imageSize.height * scale) / 2
    )
}

/// Every stroke drawn so far, plus the one still under the finger. A value
/// type, which is what makes undo exact: each edit produces a whole new
/// drawing, so there is never a partially-mutated canvas to recover from.
///
/// Points are stored in **image** coordinates. Compositing therefore happens at
/// the decoded photo's resolution, and drawing on a small screen never shrinks
/// the photo that gets sent.
struct MarkupDrawing: Equatable {
    private(set) var strokes: [MarkupStroke] = []
    private(set) var active: MarkupStroke?

    init() {}

    /// Committed strokes plus the in-progress one, in paint order.
    var visibleStrokes: [MarkupStroke] {
        guard let active else { return strokes }
        return strokes + [active]
    }

    /// True once there is anything at all to composite.
    var hasStrokes: Bool {
        !strokes.isEmpty || active != nil
    }

    /// Whether the Undo control has anything left to take back.
    var canUndo: Bool {
        hasStrokes
    }

    mutating func begin(color: MarkupColor, thickness: MarkupThickness, at point: MarkupPoint) {
        active = MarkupStroke(color: color, thickness: thickness, points: [point])
    }

    /// Extends the in-progress stroke. A no-op if no stroke is in progress.
    mutating func extend(to point: MarkupPoint) {
        guard active != nil else { return }
        active?.points.append(point)
    }

    /// Commits the in-progress stroke. A single tap still counts -- it's a dot.
    mutating func finish() {
        guard let active else { return }
        strokes.append(active)
        self.active = nil
    }

    /// Removes exactly one complete stroke -- the most recent one. An
    /// in-progress stroke is dropped first, so undo can never leave half a
    /// scribble behind. Repeatable back to a clean image; there is no redo.
    mutating func undo() {
        if active != nil {
            active = nil
            return
        }
        if strokes.isEmpty { return }
        strokes.removeLast()
    }

    /// Empties the canvas. No prompt: cancelling out of the editor undoes it.
    mutating func clear() {
        strokes = []
        active = nil
    }

    /// A copy with the in-progress stroke committed, for the confirm path.
    func finished() -> MarkupDrawing {
        var copy: MarkupDrawing = self
        copy.finish()
        return copy
    }
}

/// What confirming should actually do.
///
/// With nothing drawn the staged bytes are handed straight back: no decode, no
/// re-encode, no orientation pass. That is what keeps "open the editor, change
/// nothing, confirm" byte-identical -- and keeps an upright photo upright,
/// since it never touches the pixels at all.
enum MarkupConfirmPlan {
    case keepOriginal
    case compositeAndReencode
}

func markupConfirmPlan(drawing: MarkupDrawing) -> MarkupConfirmPlan {
    drawing.hasStrokes ? MarkupConfirmPlan.compositeAndReencode : MarkupConfirmPlan.keepOriginal
}

/// The staging size guard, re-run on the annotated result. Strokes add detail,
/// detail costs bytes, and a photo that fit before annotation can stop fitting
/// after it. `tooLarge` means the caller must show the same plain-speech
/// warning an oversized photo gets today -- never a silent drop.
enum MarkupSizeVerdict {
    case fits
    case tooLarge
}

/// `encodedBytes` is the size of the re-encoded JPEG, or nil when compression
/// gave up entirely. `maxBlobBytes` is the attachment ceiling
/// (`AttachmentPayload.maxBlobBytes`).
func markupSizeVerdict(encodedBytes: Int?, maxBlobBytes: Int) -> MarkupSizeVerdict {
    guard let encodedBytes, encodedBytes > 0, encodedBytes <= maxBlobBytes else {
        return MarkupSizeVerdict.tooLarge
    }
    return MarkupSizeVerdict.fits
}
