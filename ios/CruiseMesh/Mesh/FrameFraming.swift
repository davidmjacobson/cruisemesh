import Foundation

enum FrameFraming {
    static let attHeaderOverhead = 3
    static let defaultAttMtu = 23
    private static let headerSize = 2
    private static let maxFragments = 255

    static func fragment(frame: Data, mtuPayloadSize: Int) -> [Data] {
        let chunkSize = max(1, mtuPayloadSize - headerSize)
        let total = max(1, (frame.count + chunkSize - 1) / chunkSize)
        precondition(total <= maxFragments, "frame too large to fragment")
        return (0..<total).map { index in
            let start = index * chunkSize
            let end = min(start + chunkSize, frame.count)
            var out = Data(capacity: headerSize + (end - start))
            out.append(UInt8(index))
            out.append(UInt8(total))
            out.append(frame.subdata(in: start..<end))
            return out
        }
    }
}

final class FrameReassembler {
    private var buffer = Data()
    private var expectedTotal = 0
    private var nextIndex = 0
    private var active = false

    func accept(_ fragment: Data) -> Data? {
        guard fragment.count >= 2 else { return nil }
        let index = Int(fragment[0])
        let total = Int(fragment[1])
        if index == 0 {
            buffer = Data()
            expectedTotal = total
            nextIndex = 0
            active = true
        }
        guard active, index == nextIndex, total == expectedTotal else {
            active = false
            return nil
        }
        buffer.append(fragment.subdata(in: 2..<fragment.count))
        nextIndex += 1
        if nextIndex < expectedTotal { return nil }
        active = false
        let frame = buffer
        buffer = Data()
        return frame
    }
}
