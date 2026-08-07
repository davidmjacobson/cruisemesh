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

    /// Drives the same relocate helper restore uses after sanitize: place a
    /// staged DB at the pending path by move (not by loading into `Data`).
    func testRelocateStagedDatabaseMovesWithoutLeavingSource() throws {
        let manager = FileManager.default
        let dir = manager.temporaryDirectory
            .appendingPathComponent("backup-relocate-\(UUID().uuidString)", isDirectory: true)
        try manager.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? manager.removeItem(at: dir) }

        let staged = dir.appendingPathComponent("staged.sqlite")
        let pending = dir.appendingPathComponent("pending.sqlite")
        // Multi-megabyte payload: enough to make a mistaken Data(contentsOf:)
        // path expensive, without needing a real SQLite header for this step.
        let payload = Data(repeating: 0xA5, count: 2 * 1024 * 1024)
        try payload.write(to: staged)

        try BackupService.relocateStagedDatabase(from: staged, to: pending, fileManager: manager)

        XCTAssertFalse(manager.fileExists(atPath: staged.path), "move should consume the staged file")
        XCTAssertTrue(manager.fileExists(atPath: pending.path))
        XCTAssertEqual(try Data(contentsOf: pending), payload)
    }

    func testRelocateStagedDatabaseOverwritesAnExistingPendingFile() throws {
        let manager = FileManager.default
        let dir = manager.temporaryDirectory
            .appendingPathComponent("backup-relocate-overwrite-\(UUID().uuidString)", isDirectory: true)
        try manager.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? manager.removeItem(at: dir) }

        let staged = dir.appendingPathComponent("staged.sqlite")
        let pending = dir.appendingPathComponent("pending.sqlite")
        try Data("old".utf8).write(to: pending)
        try Data("new".utf8).write(to: staged)

        try BackupService.relocateStagedDatabase(from: staged, to: pending, fileManager: manager)

        XCTAssertEqual(try String(contentsOf: pending, encoding: .utf8), "new")
    }
}
