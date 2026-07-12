import Foundation
import UIKit

enum ProfilePhotoStore {
    private static let localEdge: CGFloat = 512
    private static let wireEdge: CGFloat = 256
    private static let wireMaxBytes = 24 * 1024

    private static var avatarURL: URL {
        let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("profile", isDirectory: true)
        return dir.appendingPathComponent("avatar.jpg")
    }

    static func loadAvatarImage() -> UIImage? {
        UIImage(contentsOfFile: avatarURL.path)
    }

    static func save(image: UIImage) -> UIImage? {
        let normalized = image.centerCropped(to: localEdge)
        guard let data = normalized.jpegData(compressionQuality: 0.9) else { return nil }
        do {
            try FileManager.default.createDirectory(
                at: avatarURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            try data.write(to: avatarURL, options: .atomic)
            return normalized
        } catch {
            return nil
        }
    }

    static func clear() {
        try? FileManager.default.removeItem(at: avatarURL)
    }

    static func loadWireAvatarBytes() -> Data {
        guard let image = loadAvatarImage() else { return Data() }
        return encodeWireAvatar(image)
    }

    static func encodeWireAvatar(_ image: UIImage) -> Data {
        let normalized = image.centerCropped(to: wireEdge)
        for quality in [0.85, 0.70, 0.55, 0.40] {
            if let data = normalized.jpegData(compressionQuality: quality),
               data.count <= wireMaxBytes {
                return data
            }
        }
        return Data()
    }
}

private extension UIImage {
    func centerCropped(to edge: CGFloat) -> UIImage {
        let side = min(size.width, size.height)
        let origin = CGPoint(x: (size.width - side) / 2, y: (size.height - side) / 2)
        let cropRect = CGRect(origin: origin, size: CGSize(width: side, height: side))
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        return UIGraphicsImageRenderer(size: CGSize(width: edge, height: edge), format: format).image { _ in
            draw(in: CGRect(
                x: -cropRect.origin.x * edge / side,
                y: -cropRect.origin.y * edge / side,
                width: size.width * edge / side,
                height: size.height * edge / side
            ))
        }
    }
}
