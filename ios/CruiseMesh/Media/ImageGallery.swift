import Foundation
import Photos
import UIKit

/// Saves chat JPEGs into the user's photo library (Android `ImageGallery` parity).
enum ImageGallery {
    enum SaveResult {
        case saved
        case denied
        case failed(String)
    }

    /// Requests add-only photo library access if needed, then writes the JPEG.
    static func saveJpeg(_ data: Data, completion: @escaping (SaveResult) -> Void) {
        guard UIImage(data: data) != nil else {
            completion(.failed("Could not decode image"))
            return
        }

        let status = PHPhotoLibrary.authorizationStatus(for: .addOnly)
        switch status {
        case .authorized, .limited:
            performSave(data, completion: completion)
        case .notDetermined:
            PHPhotoLibrary.requestAuthorization(for: .addOnly) { newStatus in
                DispatchQueue.main.async {
                    if newStatus == .authorized || newStatus == .limited {
                        performSave(data, completion: completion)
                    } else {
                        completion(.denied)
                    }
                }
            }
        case .denied, .restricted:
            completion(.denied)
        @unknown default:
            completion(.denied)
        }
    }

    private static func performSave(_ data: Data, completion: @escaping (SaveResult) -> Void) {
        PHPhotoLibrary.shared().performChanges({
            let request = PHAssetCreationRequest.forAsset()
            request.addResource(with: .photo, data: data, options: nil)
        }, completionHandler: { success, error in
            DispatchQueue.main.async {
                if success {
                    completion(.saved)
                } else {
                    completion(.failed(error?.localizedDescription ?? "Save failed"))
                }
            }
        })
    }
}
