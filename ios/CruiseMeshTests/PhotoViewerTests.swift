import XCTest
@testable import CruiseMesh

final class PhotoViewerTests: XCTestCase {
    func testScaleClampsToViewerRange() {
        XCTAssertEqual(clampedPhotoViewerScale(0.2), 1)
        XCTAssertEqual(clampedPhotoViewerScale(2.5), 2.5)
        XCTAssertEqual(clampedPhotoViewerScale(9), 5)
    }

    func testAspectFitPreservesImageRatio() {
        let fitted = aspectFitPhotoSize(
            imageSize: CGSize(width: 400, height: 200),
            containerSize: CGSize(width: 300, height: 600)
        )
        XCTAssertEqual(fitted.width, 300, accuracy: 0.001)
        XCTAssertEqual(fitted.height, 150, accuracy: 0.001)
    }

    func testOffsetIsZeroAtBaseScaleAndBoundedWhenZoomed() {
        let image = CGSize(width: 400, height: 200)
        let container = CGSize(width: 300, height: 600)

        XCTAssertEqual(
            clampedPhotoViewerOffset(
                CGSize(width: 80, height: 80),
                imageSize: image,
                containerSize: container,
                scale: 1
            ),
            .zero
        )

        let zoomed = clampedPhotoViewerOffset(
            CGSize(width: 1_000, height: -1_000),
            imageSize: image,
            containerSize: container,
            scale: 3
        )
        XCTAssertEqual(zoomed.width, 300, accuracy: 0.001)
        XCTAssertEqual(zoomed.height, 0, accuracy: 0.001)
    }

    func testDismissRequiresDownwardVerticalDragAtBaseScale() {
        XCTAssertTrue(shouldDismissPhotoViewer(
            translation: CGSize(width: 10, height: 140),
            scale: 1
        ))
        XCTAssertFalse(shouldDismissPhotoViewer(
            translation: CGSize(width: 10, height: 140),
            scale: 2
        ))
        XCTAssertFalse(shouldDismissPhotoViewer(
            translation: CGSize(width: 140, height: 130),
            scale: 1
        ))
        XCTAssertFalse(shouldDismissPhotoViewer(
            translation: CGSize(width: 0, height: -200),
            scale: 1
        ))
    }
}
