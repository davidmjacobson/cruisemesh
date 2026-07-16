import SwiftUI
import UIKit

private let photoViewerMinimumScale: CGFloat = 1
private let photoViewerMaximumScale: CGFloat = 5
private let photoViewerDoubleTapScale: CGFloat = 2.5
private let photoViewerDismissDistance: CGFloat = 120

internal func clampedPhotoViewerScale(_ proposed: CGFloat) -> CGFloat {
    min(max(proposed, photoViewerMinimumScale), photoViewerMaximumScale)
}

internal func aspectFitPhotoSize(imageSize: CGSize, containerSize: CGSize) -> CGSize {
    guard imageSize.width > 0, imageSize.height > 0,
          containerSize.width > 0, containerSize.height > 0 else {
        return .zero
    }
    let ratio = min(containerSize.width / imageSize.width, containerSize.height / imageSize.height)
    return CGSize(width: imageSize.width * ratio, height: imageSize.height * ratio)
}

internal func clampedPhotoViewerOffset(
    _ proposed: CGSize,
    imageSize: CGSize,
    containerSize: CGSize,
    scale: CGFloat
) -> CGSize {
    let fitted = aspectFitPhotoSize(imageSize: imageSize, containerSize: containerSize)
    let safeScale = clampedPhotoViewerScale(scale)
    let maxX = max(0, (fitted.width * safeScale - containerSize.width) / 2)
    let maxY = max(0, (fitted.height * safeScale - containerSize.height) / 2)
    return CGSize(
        width: min(max(proposed.width, -maxX), maxX),
        height: min(max(proposed.height, -maxY), maxY)
    )
}

internal func shouldDismissPhotoViewer(translation: CGSize, scale: CGFloat) -> Bool {
    scale <= photoViewerMinimumScale
        && translation.height > photoViewerDismissDistance
        && abs(translation.height) > abs(translation.width)
}

struct PhotoViewerOverlay: View {
    let jpeg: Data

    @Environment(\.dismiss) private var dismiss
    @State private var scale: CGFloat = photoViewerMinimumScale
    @State private var settledScale: CGFloat = photoViewerMinimumScale
    @State private var offset: CGSize = .zero
    @State private var settledOffset: CGSize = .zero
    @State private var dismissOffset: CGFloat = 0
    @State private var statusMessage: String?

    private var image: UIImage? { UIImage(data: jpeg) }

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                Color.black
                    .ignoresSafeArea()

                if let image {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFit()
                        .scaleEffect(scale)
                        .offset(x: offset.width, y: offset.height + dismissOffset)
                        .opacity(dismissOpacity)
                        .contentShape(Rectangle())
                        .onTapGesture(count: 2) {
                            toggleDoubleTapZoom(
                                imageSize: image.size,
                                containerSize: geometry.size
                            )
                        }
                        .simultaneousGesture(
                            magnificationGesture(
                                imageSize: image.size,
                                containerSize: geometry.size
                            )
                        )
                        .simultaneousGesture(
                            dragGesture(
                                imageSize: image.size,
                                containerSize: geometry.size
                            )
                        )
                        .accessibilityLabel("Full-screen photo")
                        .accessibilityHint("Pinch or double-tap to zoom; swipe down to close")
                } else {
                    VStack(spacing: 12) {
                        Image(systemName: "photo.badge.exclamationmark")
                            .font(.system(size: 44))
                        Text("Could not display photo")
                            .font(.headline)
                    }
                    .foregroundStyle(.white)
                }

                VStack {
                    HStack {
                        viewerButton(systemImage: "xmark", label: "Close") {
                            dismiss()
                        }
                        Spacer()
                        viewerButton(systemImage: "square.and.arrow.down", label: "Save image") {
                            savePhoto()
                        }
                        .disabled(image == nil)
                    }
                    Spacer()
                }
                .padding(.horizontal, 16)
                .padding(.top, 8)
            }
        }
        .statusBarHidden(true)
        .preferredColorScheme(.dark)
        .alert("Photo", isPresented: Binding(
            get: { statusMessage != nil },
            set: { if !$0 { statusMessage = nil } }
        )) {
            Button("OK", role: .cancel) { statusMessage = nil }
        } message: {
            Text(statusMessage ?? "")
        }
    }

    private var dismissOpacity: Double {
        max(0.35, 1 - Double(max(0, dismissOffset) / 500))
    }

    private func viewerButton(
        systemImage: String,
        label: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 44, height: 44)
                .background(.black.opacity(0.55), in: Circle())
        }
        .accessibilityLabel(label)
    }

    private func magnificationGesture(imageSize: CGSize, containerSize: CGSize) -> some Gesture {
        MagnificationGesture()
            .onChanged { value in
                scale = clampedPhotoViewerScale(settledScale * value)
                offset = clampedPhotoViewerOffset(
                    offset,
                    imageSize: imageSize,
                    containerSize: containerSize,
                    scale: scale
                )
            }
            .onEnded { value in
                scale = clampedPhotoViewerScale(settledScale * value)
                settledScale = scale
                offset = clampedPhotoViewerOffset(
                    offset,
                    imageSize: imageSize,
                    containerSize: containerSize,
                    scale: scale
                )
                settledOffset = offset
                if scale == photoViewerMinimumScale {
                    withAnimation(.spring(response: 0.25, dampingFraction: 0.85)) {
                        offset = .zero
                        settledOffset = .zero
                    }
                }
            }
    }

    private func dragGesture(imageSize: CGSize, containerSize: CGSize) -> some Gesture {
        DragGesture(minimumDistance: 8)
            .onChanged { value in
                if scale <= photoViewerMinimumScale {
                    dismissOffset = max(0, value.translation.height)
                } else {
                    offset = clampedPhotoViewerOffset(
                        CGSize(
                            width: settledOffset.width + value.translation.width,
                            height: settledOffset.height + value.translation.height
                        ),
                        imageSize: imageSize,
                        containerSize: containerSize,
                        scale: scale
                    )
                }
            }
            .onEnded { value in
                if shouldDismissPhotoViewer(translation: value.translation, scale: scale) {
                    dismiss()
                    return
                }
                withAnimation(.spring(response: 0.25, dampingFraction: 0.85)) {
                    dismissOffset = 0
                }
                if scale > photoViewerMinimumScale {
                    offset = clampedPhotoViewerOffset(
                        offset,
                        imageSize: imageSize,
                        containerSize: containerSize,
                        scale: scale
                    )
                    settledOffset = offset
                }
            }
    }

    private func toggleDoubleTapZoom(imageSize: CGSize, containerSize: CGSize) {
        withAnimation(.spring(response: 0.28, dampingFraction: 0.86)) {
            if scale > photoViewerMinimumScale {
                scale = photoViewerMinimumScale
                settledScale = photoViewerMinimumScale
                offset = .zero
                settledOffset = .zero
            } else {
                scale = photoViewerDoubleTapScale
                settledScale = photoViewerDoubleTapScale
                offset = clampedPhotoViewerOffset(
                    .zero,
                    imageSize: imageSize,
                    containerSize: containerSize,
                    scale: scale
                )
                settledOffset = offset
            }
        }
    }

    private func savePhoto() {
        ImageGallery.saveJpeg(jpeg) { result in
            switch result {
            case .saved:
                statusMessage = "Saved to Photos"
            case .denied:
                statusMessage = "Photo Library access is required to save images. Enable it in Settings."
            case .failed(let message):
                statusMessage = message
            }
        }
    }
}
