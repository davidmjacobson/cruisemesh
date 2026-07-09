import ImageIO
import UIKit

enum MediaCompressor {
    private static let maxEdge: CGFloat = 1280
    private static let startQuality: CGFloat = 0.82
    private static let minQuality: CGFloat = 0.40

    static func compressImage(data: Data) -> Data? {
        guard let image = UIImage(data: data) else { return nil }
        return compress(image: image)
    }

    static func compressImage(url: URL) -> Data? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return compressImage(data: data)
    }

    static func compress(image: UIImage) -> Data? {
        let scaled = scaleToMaxEdge(image, maxEdge: maxEdge)
        var quality = startQuality
        var bytes: Data?
        repeat {
            bytes = scaled.jpegData(compressionQuality: quality)
            if let bytes, bytes.count <= AttachmentPayload.maxBlobBytes {
                return bytes
            }
            quality -= 0.08
        } while quality >= minQuality
        return nil
    }

    private static func scaleToMaxEdge(_ image: UIImage, maxEdge: CGFloat) -> UIImage {
        let size = image.size
        let longest = max(size.width, size.height)
        guard longest > maxEdge else { return image }
        let scale = maxEdge / longest
        let newSize = CGSize(width: size.width * scale, height: size.height * scale)
        let renderer = UIGraphicsImageRenderer(size: newSize)
        return renderer.image { _ in
            image.draw(in: CGRect(origin: .zero, size: newSize))
        }
    }
}
