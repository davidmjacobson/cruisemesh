import XCTest
@testable import CruiseMesh

final class FrameFramingTests: XCTestCase {
    func testFragmentAndReassemble() {
        let frame = Data((0..<500).map { UInt8($0 % 251) })
        let fragments = FrameFraming.fragment(frame: frame, mtuPayloadSize: 50)
        XCTAssertGreaterThan(fragments.count, 1)
        let reassembler = FrameReassembler()
        var result: Data?
        for f in fragments {
            result = reassembler.accept(f)
        }
        XCTAssertEqual(result, frame)
    }
}
