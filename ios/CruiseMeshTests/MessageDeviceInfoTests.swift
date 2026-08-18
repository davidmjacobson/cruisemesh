import XCTest
@testable import CruiseMesh

/// §8's receipt rule at the one place it is allowed to be visible, and the
/// suppression that keeps a single-device contact's device count invisible.
///
/// The Swift twin of Android's `MessageDeviceInfoTest`.
final class MessageDeviceInfoTests: XCTestCase {
    private let first = Data([0x01, 0x01, 0x01, 0x01])
    private let second = Data([0x02, 0x02, 0x02, 0x02])
    private let stranger = Data([0x09, 0x09, 0x09, 0x09])

    func testAReceivedMessageNamesWhichOfTheirDevicesSentIt() {
        let label = deviceLabelFor(
            senderDeviceId: second,
            activeDeviceIds: [first, second],
            state: .active
        )
        XCTAssertEqual(label, .numbered(position: 2))
        XCTAssertEqual(
            messageDeviceInfoLines(isOwn: false, label: label, contactDeviceCount: 2),
            [.sentFrom(.numbered(position: 2))]
        )
    }

    /// DL-4 keeps tombstones, so a message from a device they have since removed
    /// is nameable as exactly that rather than as an unknown.
    func testADeviceTheyRemovedIsNamedAsRemoved() {
        let label = deviceLabelFor(
            senderDeviceId: stranger,
            activeDeviceIds: [first],
            state: .revoked
        )
        XCTAssertEqual(label, .removed)
        XCTAssertEqual(
            messageDeviceInfoLines(isOwn: false, label: label, contactDeviceCount: 1),
            [.sentFrom(.removed)]
        )
    }

    /// Every build in the field today sends `coreLegacyDeviceId()` and has never
    /// told us a device list. That is not an error and it is not "unknown device"
    /// either — it is "we were never told", said once.
    func testAContactWhoNeverToldUsGetsOneHonestLine() {
        let label = deviceLabelFor(senderDeviceId: nil, activeDeviceIds: [], state: .unknown)
        XCTAssertEqual(label, .unknown)
        XCTAssertEqual(
            messageDeviceInfoLines(isOwn: false, label: label, contactDeviceCount: 0),
            [.noDeviceDetail]
        )
    }

    /// A contact whose list we hold, but whose sending device is not on it, gets
    /// no line at all: there is nothing true and useful to say.
    func testAnUnknownDeviceFromAKnownListSaysNothing() {
        let label = deviceLabelFor(
            senderDeviceId: stranger,
            activeDeviceIds: [first, second],
            state: .unknown
        )
        XCTAssertEqual(label, .unknown)
        XCTAssertTrue(
            messageDeviceInfoLines(isOwn: false, label: label, contactDeviceCount: 2).isEmpty
        )
    }

    func testASentMessageSaysHowManyDevicesATickCovers() {
        XCTAssertEqual(
            messageDeviceInfoLines(isOwn: true, label: .unknown, contactDeviceCount: 3),
            [.addressedTo(deviceCount: 3)]
        )
    }

    /// §2 goal 1: a person with one device is the single-device world this whole
    /// spec promises to keep invisible. "Any of their 1 devices" would leak the
    /// count in the one case where there is nothing to know.
    func testASingleDeviceContactLeaksNoCount() {
        XCTAssertTrue(
            messageDeviceInfoLines(isOwn: true, label: .unknown, contactDeviceCount: 1).isEmpty
        )
        XCTAssertTrue(
            messageDeviceInfoLines(isOwn: true, label: .unknown, contactDeviceCount: 0).isEmpty
        )
    }

    /// The rows a person actually reads, so a line that maps to no words is a
    /// test failure rather than an empty row on a sheet.
    func testEveryLineRendersSomething() {
        let rows = deviceInfoRows([
            .sentFrom(.numbered(position: 2)),
            .sentFrom(.removed),
            .sentFrom(.unknown),
            .addressedTo(deviceCount: 4),
            .noDeviceDetail,
        ])
        XCTAssertEqual(rows.count, 5)
        for row in rows {
            switch row {
            case .labeled(let label, let value):
                XCTAssertFalse(label.isEmpty)
                XCTAssertFalse(value.isEmpty)
            case .sentence(let text):
                XCTAssertFalse(text.isEmpty)
            }
        }
    }
}
