import XCTest
@testable import CruiseMesh

final class UserIdHexTests: XCTestCase {
    func testEncodesBytesAsLowercaseTwoDigitHex() {
        let bytes = Data([0x00, 0x0A, 0xFF, 0x7B])
        XCTAssertEqual(UserIdHex.encode(bytes), "000aff7b")
    }

    func testDecodeIsInverseOfEncode() throws {
        let bytes = Data([0x00, 0x0A, 0xFF, 0x7B])
        XCTAssertEqual(try UserIdHex.decode(UserIdHex.encode(bytes)), bytes)
    }

    func testRoundTripsRandom16ByteUserId() throws {
        var bytes = [UInt8](repeating: 0, count: 16)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        let data = Data(bytes)
        XCTAssertEqual(try UserIdHex.decode(UserIdHex.encode(data)), data)
    }

    func testEmptyRoundTrip() throws {
        XCTAssertEqual(UserIdHex.encode(Data()), "")
        XCTAssertEqual(try UserIdHex.decode(""), Data())
    }

    func testDecodeRejectsOddLengthHex() {
        XCTAssertThrowsError(try UserIdHex.decode("abc"))
    }
}
