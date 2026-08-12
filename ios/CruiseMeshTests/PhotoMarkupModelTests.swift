import XCTest
@testable import CruiseMesh

/// Rules of the pre-send photo markup editor (specs/photo-markup.md). Mirrors
/// the Android suite in `PhotoMarkupModelTest.kt` case for case.
final class PhotoMarkupModelTests: XCTestCase {

    // MARK: - Coordinate mapping

    func testWidePhotoLetterboxesTopAndBottomAndMapsTouchesBackToPixels() {
        // 1000x500 image in a 500x500 frame: fits on width, 125pt bars above
        // and below.
        let fit = markupFit(
            imageSize: CGSize(width: 1_000, height: 500),
            viewSize: CGSize(width: 500, height: 500)
        )

        XCTAssertEqual(fit.scale, 0.5, accuracy: 0.0001)
        XCTAssertEqual(fit.offsetX, 0, accuracy: 0.0001)
        XCTAssertEqual(fit.offsetY, 125, accuracy: 0.0001)

        // A finger in the middle of the frame is the middle of the photo.
        let center = fit.toImagePoint(MarkupPoint(x: 250, y: 250))
        XCTAssertEqual(center.x, 500, accuracy: 0.0001)
        XCTAssertEqual(center.y, 250, accuracy: 0.0001)

        // Top-left of the drawn photo is pixel (0, 0), not the frame corner.
        let corner = fit.toImagePoint(MarkupPoint(x: 0, y: 125))
        XCTAssertEqual(corner.x, 0, accuracy: 0.0001)
        XCTAssertEqual(corner.y, 0, accuracy: 0.0001)
    }

    func testTallPhotoLetterboxesLeftAndRight() {
        let fit = markupFit(
            imageSize: CGSize(width: 400, height: 800),
            viewSize: CGSize(width: 400, height: 400)
        )

        XCTAssertEqual(fit.scale, 0.5, accuracy: 0.0001)
        XCTAssertEqual(fit.offsetX, 100, accuracy: 0.0001)
        XCTAssertEqual(fit.offsetY, 0, accuracy: 0.0001)
        XCTAssertEqual(fit.toImagePoint(MarkupPoint(x: 200, y: 300)).y, 600, accuracy: 0.0001)
    }

    func testViewToImageAndBackIsARoundTrip() {
        let fit = markupFit(
            imageSize: CGSize(width: 1_024, height: 768),
            viewSize: CGSize(width: 360, height: 640)
        )
        let touch = MarkupPoint(x: 123.5, y: 301.25)

        let roundTripped = fit.toViewPoint(fit.toImagePoint(touch))

        XCTAssertEqual(roundTripped.x, touch.x, accuracy: 0.001)
        XCTAssertEqual(roundTripped.y, touch.y, accuracy: 0.001)
    }

    func testUnmeasuredFrameMapsPointsThroughUnchangedInsteadOfDividingByZero() {
        let fit = markupFit(
            imageSize: CGSize(width: 1_024, height: 768),
            viewSize: .zero
        )

        XCTAssertEqual(fit, MarkupFit.identity)
        XCTAssertEqual(fit.toImagePoint(MarkupPoint(x: 7, y: 9)), MarkupPoint(x: 7, y: 9))
    }

    func testPhotoRectIsWhereTheStrokesAreClipped() {
        let imageSize = CGSize(width: 1_000, height: 500)
        let fit = markupFit(imageSize: imageSize, viewSize: CGSize(width: 500, height: 500))

        let rect = fit.photoRect(imageSize: imageSize)

        XCTAssertEqual(rect.minX, 0, accuracy: 0.0001)
        XCTAssertEqual(rect.minY, 125, accuracy: 0.0001)
        XCTAssertEqual(rect.width, 500, accuracy: 0.0001)
        XCTAssertEqual(rect.height, 250, accuracy: 0.0001)
    }

    func testStrokesAreStoredInImagePixelsSoASmallScreenDoesNotShrinkThePhoto() {
        // Drawing on a 360x640 phone over a 1024x768 photo.
        let imageSize = CGSize(width: 1_024, height: 768)
        let fit = markupFit(imageSize: imageSize, viewSize: CGSize(width: 360, height: 640))
        var drawing = MarkupDrawing()
        drawing.begin(
            color: .red,
            thickness: .medium,
            at: fit.toImagePoint(MarkupPoint(x: 0, y: fit.offsetY))
        )
        drawing.extend(
            to: fit.toImagePoint(MarkupPoint(x: 360, y: fit.offsetY + imageSize.height * fit.scale))
        )
        drawing.finish()

        let points = drawing.strokes[0].points
        XCTAssertEqual(points.count, 2)
        XCTAssertEqual(points[0].x, 0, accuracy: 0.01)
        XCTAssertEqual(points[0].y, 0, accuracy: 0.01)
        XCTAssertEqual(points[1].x, 1_024, accuracy: 0.01)
        XCTAssertEqual(points[1].y, 768, accuracy: 0.01)
    }

    // MARK: - Undo and clear

    func testUndoRemovesExactlyOneCompleteStroke() {
        var drawing = MarkupDrawing()
        stroke(&drawing, at: 1)
        stroke(&drawing, at: 2)
        stroke(&drawing, at: 3)
        XCTAssertEqual(drawing.strokes.count, 3)

        drawing.undo()
        XCTAssertEqual(drawing.strokes.count, 2)
        XCTAssertEqual(drawing.strokes.map { $0.points[0].x }, [1, 2])

        drawing.undo()
        XCTAssertEqual(drawing.strokes.count, 1)
        XCTAssertEqual(drawing.strokes.map { $0.points[0].x }, [1])
    }

    func testUndoIsRepeatableBackToACleanImageAndThenDoesNothing() {
        var drawing = MarkupDrawing()
        stroke(&drawing, at: 1)
        stroke(&drawing, at: 2)

        for _ in 0..<4 {
            drawing.undo()
        }

        XCTAssertTrue(drawing.strokes.isEmpty)
        XCTAssertNil(drawing.active)
        XCTAssertFalse(drawing.hasStrokes)
        XCTAssertFalse(drawing.canUndo)
    }

    func testUndoDropsAnInProgressStrokeFirstSoHalfAScribbleIsNeverLeftBehind() {
        var drawing = MarkupDrawing()
        stroke(&drawing, at: 1)
        drawing.begin(color: .red, thickness: .medium, at: MarkupPoint(x: 9, y: 9))
        drawing.extend(to: MarkupPoint(x: 10, y: 10))

        drawing.undo()

        XCTAssertNil(drawing.active)
        XCTAssertEqual(drawing.strokes.count, 1)
    }

    func testClearEmptiesTheCanvasIncludingAStrokeStillUnderTheFinger() {
        var drawing = MarkupDrawing()
        stroke(&drawing, at: 1)
        stroke(&drawing, at: 2)
        drawing.begin(color: .white, thickness: .thick, at: MarkupPoint(x: 5, y: 5))

        drawing.clear()

        XCTAssertTrue(drawing.strokes.isEmpty)
        XCTAssertNil(drawing.active)
        XCTAssertFalse(drawing.hasStrokes)
        XCTAssertTrue(drawing.visibleStrokes.isEmpty)
    }

    func testAnInProgressStrokeIsVisibleBeforeItIsCommitted() {
        var drawing = MarkupDrawing()
        drawing.begin(color: .yellow, thickness: .thin, at: MarkupPoint(x: 1, y: 1))
        drawing.extend(to: MarkupPoint(x: 2, y: 2))

        XCTAssertTrue(drawing.strokes.isEmpty)
        XCTAssertEqual(drawing.visibleStrokes.count, 1)
        XCTAssertEqual(drawing.visibleStrokes[0].points.count, 2)

        drawing.finish()
        XCTAssertEqual(drawing.strokes.count, 1)
        XCTAssertNil(drawing.active)
    }

    func testASingleTapStillCountsAsAStroke() {
        var drawing = MarkupDrawing()
        drawing.begin(color: .red, thickness: .medium, at: MarkupPoint(x: 4, y: 4))
        drawing.finish()

        XCTAssertEqual(drawing.strokes.count, 1)
        XCTAssertEqual(drawing.strokes[0].points.count, 1)
    }

    func testExtendingWithNoStrokeInProgressIsANoOp() {
        var drawing = MarkupDrawing()

        drawing.extend(to: MarkupPoint(x: 3, y: 3))

        XCTAssertNil(drawing.active)
        XCTAssertFalse(drawing.hasStrokes)
    }

    func testUndoingACopyLeavesTheOriginalAlone() {
        var original = MarkupDrawing()
        stroke(&original, at: 1)
        stroke(&original, at: 2)

        var copy = original
        copy.undo()

        XCTAssertEqual(original.strokes.count, 2)
        XCTAssertEqual(copy.strokes.count, 1)
    }

    // MARK: - Confirming

    func testConfirmingWithNothingDrawnKeepsTheOriginalBytesUntouched() {
        XCTAssertEqual(markupConfirmPlan(drawing: MarkupDrawing()), MarkupConfirmPlan.keepOriginal)

        var undone = MarkupDrawing()
        stroke(&undone, at: 1)
        undone.undo()
        XCTAssertEqual(markupConfirmPlan(drawing: undone), MarkupConfirmPlan.keepOriginal)

        var cleared = MarkupDrawing()
        stroke(&cleared, at: 1)
        cleared.clear()
        XCTAssertEqual(markupConfirmPlan(drawing: cleared), MarkupConfirmPlan.keepOriginal)
    }

    func testConfirmingAfterDrawingCompositesAndReencodes() {
        var drawn = MarkupDrawing()
        stroke(&drawn, at: 1)
        XCTAssertEqual(markupConfirmPlan(drawing: drawn), MarkupConfirmPlan.compositeAndReencode)

        var inProgress = MarkupDrawing()
        inProgress.begin(color: .red, thickness: .thin, at: MarkupPoint(x: 0, y: 0))
        XCTAssertEqual(markupConfirmPlan(drawing: inProgress), MarkupConfirmPlan.compositeAndReencode)
    }

    func testFinishedCommitsTheStrokeUnderTheFingerWithoutMutatingTheSource() {
        var drawing = MarkupDrawing()
        drawing.begin(color: .red, thickness: .medium, at: MarkupPoint(x: 1, y: 1))

        let finished = drawing.finished()

        XCTAssertEqual(finished.strokes.count, 1)
        XCTAssertNil(finished.active)
        XCTAssertTrue(drawing.strokes.isEmpty)
        XCTAssertNotNil(drawing.active)
    }

    // MARK: - Size guard and pen defaults

    func testTheSizeGuardRejectsAnAnnotatedPhotoThatNoLongerFits() {
        let limit = 180 * 1_024

        XCTAssertEqual(
            markupSizeVerdict(encodedBytes: 48 * 1_024, maxBlobBytes: limit),
            MarkupSizeVerdict.fits
        )
        XCTAssertEqual(
            markupSizeVerdict(encodedBytes: limit, maxBlobBytes: limit),
            MarkupSizeVerdict.fits
        )
        XCTAssertEqual(
            markupSizeVerdict(encodedBytes: limit + 1, maxBlobBytes: limit),
            MarkupSizeVerdict.tooLarge
        )
        // Compression gave up entirely.
        XCTAssertEqual(
            markupSizeVerdict(encodedBytes: nil, maxBlobBytes: limit),
            MarkupSizeVerdict.tooLarge
        )
        // An empty encode is a failure, not a very small photo.
        XCTAssertEqual(
            markupSizeVerdict(encodedBytes: 0, maxBlobBytes: limit),
            MarkupSizeVerdict.tooLarge
        )
    }

    func testPenWidthScalesWithThePhotoSoAThickStrokeLooksTheSameOnAnyImage() {
        let small = MarkupThickness.thick.width(longestEdge: 512)
        let large = MarkupThickness.thick.width(longestEdge: 1_024)

        XCTAssertEqual(large / small, 2, accuracy: 0.0001)
        XCTAssertLessThan(
            MarkupThickness.thin.width(longestEdge: 1_024),
            MarkupThickness.medium.width(longestEdge: 1_024)
        )
        XCTAssertLessThan(
            MarkupThickness.medium.width(longestEdge: 1_024),
            MarkupThickness.thick.width(longestEdge: 1_024)
        )
        // Never a zero-width pen, however tiny the image.
        XCTAssertGreaterThanOrEqual(MarkupThickness.thin.width(longestEdge: 1), 1)
        XCTAssertGreaterThanOrEqual(MarkupThickness.thin.width(longestEdge: 0), 1)
    }

    func testRedIsTheDefaultPenAndMediumTheDefaultWidth() {
        XCTAssertEqual(MarkupColor.defaultColor, MarkupColor.red)
        XCTAssertEqual(MarkupThickness.defaultThickness, MarkupThickness.medium)
    }

    func testEveryPenColorIsFullyOpaqueAndDistinct() {
        let colors = MarkupColor.allCases
        XCTAssertEqual(colors.count, 4)
        XCTAssertEqual(Set(colors.map { $0.rawValue }).count, colors.count)
        for swatch in colors {
            XCTAssertEqual(swatch.components.alpha, 1, accuracy: 0.0001)
        }
        for (index, swatch) in colors.enumerated() {
            for other in colors[(index + 1)...] {
                XCTAssertNotEqual(swatch.components, other.components)
            }
        }
    }

    func testEveryThicknessStepIsDistinctAndOrdered() {
        let steps = MarkupThickness.allCases
        XCTAssertEqual(steps.count, 3)
        XCTAssertEqual(steps, [.thin, .medium, .thick])
    }

    // MARK: - Helpers

    private func stroke(_ drawing: inout MarkupDrawing, at x: CGFloat) {
        drawing.begin(color: .red, thickness: .medium, at: MarkupPoint(x: x, y: x))
        drawing.extend(to: MarkupPoint(x: x + 1, y: x + 1))
        drawing.finish()
    }
}
