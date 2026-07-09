import Foundation

enum UserIdHex {
    static func encode(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }

    static func encode(_ bytes: [UInt8]) -> String {
        encode(Data(bytes))
    }

    static func decode(_ hex: String) throws -> Data {
        let cleaned = hex.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard cleaned.count % 2 == 0 else {
            throw NSError(domain: "UserIdHex", code: 1, userInfo: [NSLocalizedDescriptionKey: "odd hex length"])
        }
        var data = Data()
        data.reserveCapacity(cleaned.count / 2)
        var index = cleaned.startIndex
        while index < cleaned.endIndex {
            let next = cleaned.index(index, offsetBy: 2)
            let byteStr = cleaned[index..<next]
            guard let byte = UInt8(byteStr, radix: 16) else {
                throw NSError(domain: "UserIdHex", code: 2, userInfo: [NSLocalizedDescriptionKey: "bad hex"])
            }
            data.append(byte)
            index = next
        }
        return data
    }
}

extension Data {
    func contentEquals(_ other: Data) -> Bool {
        self == other
    }

    func contentEquals(_ other: [UInt8]) -> Bool {
        self == Data(other)
    }
}
