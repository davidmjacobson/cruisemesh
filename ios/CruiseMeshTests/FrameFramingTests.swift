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

    func testCapsFragmentsAtMaximumAttributeValueLength() {
        let fragments = FrameFraming.fragment(frame: Data(repeating: 1, count: 1_500), mtuPayloadSize: 1_000)
        XCTAssertFalse(fragments.isEmpty)
        XCTAssertTrue(fragments.allSatisfy { $0.count <= FrameFraming.maxAttValueLength })
    }

    func testOversizedFrameFailsWithoutCrashing() {
        let maximumFrameSize = (FrameFraming.maxAttValueLength - 2) * 255
        let fragments = FrameFraming.fragment(
            frame: Data(repeating: 1, count: maximumFrameSize + 1),
            mtuPayloadSize: FrameFraming.maxAttValueLength
        )
        XCTAssertTrue(fragments.isEmpty)
    }

    func testRejectsInvalidZeroFragmentCount() {
        let reassembler = FrameReassembler()
        XCTAssertNil(reassembler.accept(Data([0, 0, 1, 2, 3])))
    }
}
