import Foundation

/// Wire content for `kind=16` attachment-manifest messages (DESIGN.md §8).
/// Layout matches Android `AttachmentPayload` byte-for-byte.
struct AttachmentPayload: Equatable {
    enum MediaType: UInt8 {
        case image = 1
        case audio = 2
    }

    static var maxBlobBytes: Int { Int(attachmentMaxBlobBytes()) }

    var mediaType: MediaType
    var mimeType: String
    var durationMs: Int32
    var blob: Data
    var caption: String

    init(mediaType: MediaType, mimeType: String, durationMs: Int32, blob: Data, caption: String = "") {
        self.mediaType = mediaType
        self.mimeType = mimeType
        self.durationMs = max(0, durationMs)
        self.blob = blob
        self.caption = caption
    }

    func encode() -> Data {
        try! encodeAttachmentPayload(payload: CoreAttachmentPayload(
            mediaType: mediaType == .image ? .image : .audio,
            mimeType: mimeType,
            durationMs: Int64(durationMs),
            blob: blob,
            caption: caption
        ))
    }

    static func decode(_ bytes: Data) -> AttachmentPayload? {
        guard let decoded = decodeAttachmentPayload(bytes: bytes),
              let duration = Int32(exactly: decoded.durationMs) else { return nil }
        return AttachmentPayload(
            mediaType: decoded.mediaType == .image ? .image : .audio,
            mimeType: decoded.mimeType,
            durationMs: duration,
            blob: decoded.blob,
            caption: decoded.caption
        )
    }

    static func previewLabel(_ payload: AttachmentPayload?) -> String {
        guard let payload else { return "Attachment" }
        switch payload.mediaType {
        case .image:
            return payload.caption.isEmpty ? "📷 Photo" : "📷 \(payload.caption)"
        case .audio:
            let secs = max(1, Int((payload.durationMs + 500) / 1000))
            return payload.caption.isEmpty ? "🎤 Voice memo (\(secs) s)" : "🎤 \(payload.caption)"
        }
    }

}
