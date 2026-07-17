import Foundation

enum FrameFraming {
    static var attHeaderOverhead: Int { Int(bleAttHeaderOverhead()) }
    static var defaultAttMtu: Int { Int(bleDefaultAttMtu()) }
    static var maxAttValueLength: Int { Int(bleMaxAttValueLen()) }

    static func fragment(frame: Data, mtuPayloadSize: Int) -> [Data] {
        fragmentBleFrame(frame: frame, mtuPayloadSize: UInt32(mtuPayloadSize)) ?? []
    }
}

final class FrameReassembler {
    private let core = BleFrameReassembler()

    func accept(_ fragment: Data) -> Data? {
        core.accept(fragment: fragment)
    }
}
