import SwiftUI
import UIKit

/// Full-screen "Draw" surface for a photo that is staged but not yet sent
/// (`specs/photo-markup.md`). A marker, not an art tool: freehand pen, four
/// colors, three widths, undo, clear. The Android half is
/// `chat/PhotoMarkupEditor.kt` -- same controls, same defaults, same wording.
///
/// `jpeg` is the already-compressed staged blob. `onConfirm` is called with
/// bytes that have already passed the staging size guard, so the caller can
/// stage them exactly as it stages a freshly picked photo. Confirming without
/// drawing anything hands `jpeg` straight back untouched, which is what keeps a
/// zero-stroke round trip byte-identical and an upright photo upright.
struct PhotoMarkupEditor: View {
    let jpeg: Data
    let onCancel: () -> Void
    let onConfirm: (Data) -> Void

    @State private var image: UIImage?
    @State private var decodeFailed = false
    @State private var drawing = MarkupDrawing()
    @State private var color: MarkupColor = MarkupColor.defaultColor
    @State private var thickness: MarkupThickness = MarkupThickness.defaultThickness
    @State private var working = false
    @State private var statusMessage: String?
    /// Whether a stroke is under the finger right now. Deliberately separate
    /// from `drawing.active`, which Undo and Clear can drop mid-drag: the rest
    /// of that drag must then paint nothing, rather than quietly starting a
    /// replacement stroke the user cannot take back until they lift.
    @State private var strokeUnderFinger = false
    /// Resets itself when the drag ends *or is cancelled*. `onEnded` is not
    /// delivered when the system takes the touch away (an edge gesture, an
    /// interruption), and without a cancellation signal the next unrelated
    /// touch would be appended to the abandoned stroke.
    @GestureState private var fingerDown = false

    private var undoEnabled: Bool {
        !working && drawing.canUndo
    }

    var body: some View {
        ZStack {
            Color.black
                .ignoresSafeArea()

            VStack(spacing: 0) {
                topBar

                Group {
                    if let image {
                        GeometryReader { geometry in
                            canvasLayer(image: image, viewSize: geometry.size)
                        }
                    } else if decodeFailed {
                        Text("Could not display photo")
                            .font(.headline)
                            .foregroundStyle(.white)
                            .padding(24)
                    } else {
                        Color.clear
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)

                toolBar
            }
        }
        .statusBarHidden(true)
        .preferredColorScheme(.dark)
        .onAppear {
            guard image == nil, !decodeFailed else { return }
            let decoded: UIImage? = UIImage(data: jpeg)
            image = decoded
            decodeFailed = decoded == nil
        }
        .onChange(of: fingerDown) { down in
            guard !down else { return }
            endStroke()
        }
        .alert("Photo", isPresented: Binding(
            get: { statusMessage != nil },
            set: { if !$0 { statusMessage = nil } }
        )) {
            Button("OK", role: .cancel) { statusMessage = nil }
        } message: {
            Text(statusMessage ?? "")
        }
    }

    // MARK: - Controls

    /// Cancel, undo/clear, and done sit above the drawing surface, and the pens
    /// below it, so a stroke at the edge of the photo can never land on a
    /// button.
    private var topBar: some View {
        HStack(spacing: 8) {
            // Each button carries its own 44pt target. Putting the frame on
            // the HStack instead only makes the row tall: every button still
            // hit-tests to its own text bounds, which is a ~20pt target for a
            // thumb on a moving ship.
            Button("Cancel") { onCancel() }
                .frame(minWidth: 44, minHeight: 44)
                .contentShape(Rectangle())
                .foregroundStyle(.white)
                .opacity(working ? 0.4 : 1)
                .disabled(working)

            Spacer()

            Button("Undo") { drawing.undo() }
                .frame(minWidth: 44, minHeight: 44)
                .contentShape(Rectangle())
                .foregroundStyle(.white)
                .opacity(undoEnabled ? 1 : 0.4)
                .disabled(!undoEnabled)

            Button("Clear") { drawing.clear() }
                .frame(minWidth: 44, minHeight: 44)
                .contentShape(Rectangle())
                .foregroundStyle(.white)
                .opacity(undoEnabled ? 1 : 0.4)
                .disabled(!undoEnabled)

            Spacer()

            Button {
                confirm()
            } label: {
                Text("Done")
                    .fontWeight(.semibold)
            }
            .frame(minWidth: 44, minHeight: 44)
            .contentShape(Rectangle())
            .foregroundStyle(.white)
            .opacity(working ? 0.4 : 1)
            .disabled(working)
        }
        .frame(minHeight: 44)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    /// Two rows rather than one: seven 44pt targets do not fit across a narrow
    /// phone, and shrinking them below the app's minimum is not an option for a
    /// control used with a thumb on a moving ship.
    private var toolBar: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                ForEach(MarkupColor.allCases, id: \.self) { swatch in
                    swatchButton(swatch)
                }
            }
            HStack(spacing: 0) {
                ForEach(MarkupThickness.allCases, id: \.self) { step in
                    thicknessButton(step)
                }
            }
        }
        .padding(.horizontal, 8)
        .padding(.bottom, 4)
    }

    @ViewBuilder
    private func swatchButton(_ swatch: MarkupColor) -> some View {
        let button = Button {
            color = swatch
        } label: {
            swatchCircle(swatch)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(colorLabel(swatch))

        if swatch == color {
            button.accessibilityAddTraits(.isSelected)
        } else {
            button
        }
    }

    private func swatchCircle(_ swatch: MarkupColor) -> some View {
        let selected: Bool = swatch == color
        let diameter: CGFloat = selected ? 32 : 26
        let ring: Color = selected ? Color.white : Color.white.opacity(0.35)
        let ringWidth: CGFloat = selected ? 3 : 1
        // The hit target stays at the 44pt minimum; the swatch inside it is the
        // visual size.
        return Circle()
            .fill(markupColor(swatch))
            .frame(width: diameter, height: diameter)
            .overlay {
                Circle().stroke(ring, lineWidth: ringWidth)
            }
            .frame(width: 48, height: 48)
            .contentShape(Rectangle())
    }

    @ViewBuilder
    private func thicknessButton(_ step: MarkupThickness) -> some View {
        let button = Button {
            thickness = step
        } label: {
            thicknessDot(step)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(thicknessLabel(step))

        if step == thickness {
            button.accessibilityAddTraits(.isSelected)
        } else {
            button
        }
    }

    private func thicknessDot(_ step: MarkupThickness) -> some View {
        let selected: Bool = step == thickness
        let diameter: CGFloat = markupThicknessDotDiameter(step)
        let paint: Color = selected ? Color.white : Color.white.opacity(0.4)
        return Circle()
            .fill(paint)
            .frame(width: diameter, height: diameter)
            .frame(width: 48, height: 48)
            .contentShape(Rectangle())
    }

    // MARK: - Drawing surface

    /// Draws the photo letterboxed inside the frame and the strokes on top of
    /// it, both through the same `MarkupFit`, so what the finger touches and
    /// what the eye sees can never drift apart.
    private func canvasLayer(image: UIImage, viewSize: CGSize) -> some View {
        let imageSize: CGSize = image.size
        let fit: MarkupFit = markupFit(imageSize: imageSize, viewSize: viewSize)
        let photoRect: CGRect = fit.photoRect(imageSize: imageSize)
        let edge: Int = markupLongestEdge(of: image)
        let strokes: [MarkupStroke] = drawing.visibleStrokes

        return ZStack {
            Image(uiImage: image)
                .resizable()
                .frame(width: photoRect.width, height: photoRect.height)
                .position(x: photoRect.midX, y: photoRect.midY)

            Canvas { context, _ in
                // Strokes are clipped to the photo: a finger that slides off the
                // edge of a letterboxed image must not paint on the black
                // surround.
                context.clip(to: Path(photoRect))
                for stroke in strokes {
                    let width: CGFloat = stroke.thickness.width(longestEdge: edge) * fit.scale
                    let paint: Color = markupColor(stroke.color)
                    let viewPoints: [CGPoint] = stroke.points.map { fit.toViewPoint($0).cgPoint }
                    guard let first = viewPoints.first else { continue }
                    if viewPoints.count == 1 {
                        // A single tap is a dot, not a zero-length line.
                        let dot = CGRect(
                            x: first.x - width / 2,
                            y: first.y - width / 2,
                            width: width,
                            height: width
                        )
                        context.fill(Path(ellipseIn: dot), with: .color(paint))
                        continue
                    }
                    var path = Path()
                    path.move(to: first)
                    for point in viewPoints.dropFirst() {
                        path.addLine(to: point)
                    }
                    context.stroke(
                        path,
                        with: .color(paint),
                        style: StrokeStyle(lineWidth: width, lineCap: .round, lineJoin: .round)
                    )
                }
            }
            .allowsHitTesting(false)
        }
        .contentShape(Rectangle())
        .gesture(drawGesture(fit: fit))
        .accessibilityElement()
        .accessibilityLabel("Drawing area")
    }

    /// `minimumDistance: 0` so a tap registers as a dot the same way a drag
    /// registers as a line -- there is no separate tap recognizer to keep in
    /// step with this one.
    private func drawGesture(fit: MarkupFit) -> some Gesture {
        DragGesture(minimumDistance: 0, coordinateSpace: .local)
            .updating($fingerDown) { _, state, _ in state = true }
            .onChanged { value in
                guard !working else { return }
                let point: MarkupPoint = fit.toImagePoint(MarkupPoint(value.location))
                // Whether this touch continues a stroke is a question about the
                // finger, not about `drawing.active` -- Undo mid-drag empties
                // the latter, and `extend` then correctly paints nothing for the
                // rest of the drag instead of re-beginning the stroke.
                if strokeUnderFinger {
                    drawing.extend(to: point)
                } else {
                    strokeUnderFinger = true
                    drawing.begin(color: color, thickness: thickness, at: point)
                }
            }
            .onEnded { _ in
                endStroke()
            }
    }

    /// Commits whatever is under the finger and forgets it. Idempotent, because
    /// it is reached both from `onEnded` and from the cancellation reset of
    /// `fingerDown`, and either can arrive first -- or, on a cancelled gesture,
    /// only the second.
    private func endStroke() {
        strokeUnderFinger = false
        drawing.finish()
    }

    // MARK: - Confirm

    private func confirm() {
        guard !working else { return }
        let finished: MarkupDrawing = drawing.finished()
        guard markupConfirmPlan(drawing: finished) == MarkupConfirmPlan.compositeAndReencode,
              let source = image else {
            onConfirm(jpeg)
            return
        }
        working = true
        let strokes: [MarkupStroke] = finished.strokes
        let edge: Int = markupLongestEdge(of: source)
        let maxBytes: Int = AttachmentPayload.maxBlobBytes
        DispatchQueue.global(qos: .userInitiated).async {
            let annotated: Data? = compositeMarkup(image: source, strokes: strokes, longestEdge: edge)
            DispatchQueue.main.async {
                working = false
                // Strokes add detail and detail costs bytes, so a photo that fit
                // before annotation can stop fitting after it. Same warning an
                // oversized photo gets today, and the editor stays open so the
                // strokes can be undone rather than lost.
                guard
                    markupSizeVerdict(
                        encodedBytes: annotated?.count,
                        maxBlobBytes: maxBytes
                    ) == MarkupSizeVerdict.fits,
                    let annotated
                else {
                    statusMessage = String(localized: "Could not prepare photo")
                    return
                }
                onConfirm(annotated)
            }
        }
    }

    // MARK: - Copy

    private func colorLabel(_ swatch: MarkupColor) -> Text {
        switch swatch {
        case .red:
            return Text("Red")
        case .yellow:
            return Text("Yellow")
        case .white:
            return Text("White")
        case .black:
            return Text("Black")
        }
    }

    private func thicknessLabel(_ step: MarkupThickness) -> Text {
        switch step {
        case .thin:
            return Text("Thin pen")
        case .medium:
            return Text("Medium pen")
        case .thick:
            return Text("Thick pen")
        }
    }
}

/// The one place the framework-free model crosses into SwiftUI's color type.
private func markupColor(_ swatch: MarkupColor) -> Color {
    let components: MarkupRGBA = swatch.components
    return Color(
        .sRGB,
        red: components.red,
        green: components.green,
        blue: components.blue,
        opacity: components.alpha
    )
}

/// Visual size of the thickness picker's dots. Only an affordance -- the pen
/// width itself comes from `MarkupThickness.width(longestEdge:)`.
private func markupThicknessDotDiameter(_ step: MarkupThickness) -> CGFloat {
    switch step {
    case .thin:
        return 8
    case .medium:
        return 14
    case .thick:
        return 20
    }
}

/// `UIImage.size` is already orientation-corrected, so this is the longest edge
/// of the photo as the user sees it -- the same edge the composite is drawn at.
private func markupLongestEdge(of image: UIImage) -> Int {
    Int(max(image.size.width, image.size.height).rounded())
}

/// Paints `strokes` onto `image` at the decoded photo's own resolution and
/// re-encodes through the staging compression path. Stroke points are already
/// in image coordinates, so nothing here depends on how big the phone screen
/// was. Returns nil if the result cannot be made to fit, which the caller turns
/// into the oversized-photo warning.
///
/// `image.draw(in:)` renders through the image's orientation, and the renderer
/// hands back an `.up` image at scale 1 -- so the composite is upright with no
/// EXIF pass of its own, exactly as `MediaCompressor` already behaves.
private func compositeMarkup(image: UIImage, strokes: [MarkupStroke], longestEdge: Int) -> Data? {
    let size: CGSize = image.size
    guard size.width > 0, size.height > 0 else { return nil }

    let format = UIGraphicsImageRendererFormat.default()
    format.scale = 1
    format.opaque = true
    let renderer = UIGraphicsImageRenderer(size: size, format: format)
    let annotated: UIImage = renderer.image { rendererContext in
        image.draw(in: CGRect(origin: .zero, size: size))
        let context: CGContext = rendererContext.cgContext
        context.setLineCap(.round)
        context.setLineJoin(.round)
        for stroke in strokes {
            let width: CGFloat = stroke.thickness.width(longestEdge: longestEdge)
            let components: MarkupRGBA = stroke.color.components
            let paint: CGColor = UIColor(
                red: CGFloat(components.red),
                green: CGFloat(components.green),
                blue: CGFloat(components.blue),
                alpha: CGFloat(components.alpha)
            ).cgColor
            guard let first = stroke.points.first else { continue }
            if stroke.points.count == 1 {
                context.setFillColor(paint)
                context.fillEllipse(in: CGRect(
                    x: first.x - width / 2,
                    y: first.y - width / 2,
                    width: width,
                    height: width
                ))
                continue
            }
            context.setStrokeColor(paint)
            context.setLineWidth(width)
            context.beginPath()
            context.move(to: first.cgPoint)
            for point in stroke.points.dropFirst() {
                context.addLine(to: point.cgPoint)
            }
            context.strokePath()
        }
    }
    return MediaCompressor.compress(image: annotated)
}
