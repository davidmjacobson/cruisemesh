import Foundation

/// Wire content for `kind=16` attachment-manifest messages (DESIGN.md §8).
/// Layout matches Android `AttachmentPayload` byte-for-byte.
struct AttachmentPayload: Equatable {
    enum MediaType: UInt8 {
        case image = 1
        case audio = 2
    }

    static let wireVersion: UInt8 = 1
    static let maxBlobBytes = 180 * 1024

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
        var out = Data()
        out.append(Self.wireVersion)
        out.append(mediaType.rawValue)
        Self.writeUtf16(&out, mimeType)
        out.append(contentsOf: withUnsafeBytes(of: durationMs.bigEndian, Array.init))
        let blobLen = UInt32(blob.count).bigEndian
        out.append(contentsOf: withUnsafeBytes(of: blobLen, Array.init))
        out.append(blob)
        Self.writeUtf16(&out, caption)
        return out
    }

    static func decode(_ bytes: Data) -> AttachmentPayload? {
        guard !bytes.isEmpty else { return nil }
        var offset = 0
        func need(_ n: Int) -> Bool { offset + n <= bytes.count }

        guard need(1), bytes[offset] == wireVersion else { return nil }
        offset += 1
        guard need(1), let mediaType = MediaType(rawValue: bytes[offset]) else { return nil }
        offset += 1
        guard let mime = readUtf16(bytes, &offset) else { return nil }
        guard need(4) else { return nil }
        let duration = Int32(bigEndian: bytes.subdata(in: offset..<offset+4).withUnsafeBytes { $0.load(as: Int32.self) })
        offset += 4
        guard duration >= 0 else { return nil }
        guard need(4) else { return nil }
        let blobLen = Int(UInt32(bigEndian: bytes.subdata(in: offset..<offset+4).withUnsafeBytes { $0.load(as: UInt32.self) }))
        offset += 4
        guard blobLen >= 0, blobLen <= maxBlobBytes * 2, need(blobLen) else { return nil }
        let blob = bytes.subdata(in: offset..<offset+blobLen)
        offset += blobLen
        guard let caption = readUtf16(bytes, &offset) else { return nil }
        return AttachmentPayload(
            mediaType: mediaType,
            mimeType: mime,
            durationMs: duration,
            blob: blob,
            caption: caption
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

    private static func writeUtf16(_ out: inout Data, _ value: String) {
        let encoded = Data(value.utf8)
        precondition(encoded.count <= 0xFFFF)
        let len = UInt16(encoded.count).bigEndian
        out.append(contentsOf: withUnsafeBytes(of: len, Array.init))
        out.append(encoded)
    }

    private static func readUtf16(_ bytes: Data, _ offset: inout Int) -> String? {
        guard offset + 2 <= bytes.count else { return nil }
        let len = Int(UInt16(bigEndian: bytes.subdata(in: offset..<offset+2).withUnsafeBytes { $0.load(as: UInt16.self) }))
        offset += 2
        guard offset + len <= bytes.count else { return nil }
        let slice = bytes.subdata(in: offset..<offset+len)
        offset += len
        return String(data: slice, encoding: .utf8)
    }
}
