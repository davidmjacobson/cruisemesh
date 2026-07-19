import XCTest
@testable import CruiseMesh

final class BackupServiceTests: XCTestCase {
    func testBoundedBackupReaderRejectsBytesBeyondItsLimit() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("backup-reader-\(UUID().uuidString).cmbak")
        defer { try? FileManager.default.removeItem(at: url) }
        try Data(repeating: 7, count: 9).write(to: url)

        XCTAssertThrowsError(try BackupService.readBackupFile(at: url, maxBytes: 8)) { error in
            XCTAssertEqual(error as? BackupServiceError, .fileTooLarge)
        }
    }

    func testBoundedBackupReaderAcceptsTheExactLimit() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("backup-reader-\(UUID().uuidString).cmbak")
        defer { try? FileManager.default.removeItem(at: url) }
        let expected = Data((0..<8).map { UInt8($0) })
        try expected.write(to: url)

        XCTAssertEqual(try BackupService.readBackupFile(at: url, maxBytes: 8), expected)
    }
}
