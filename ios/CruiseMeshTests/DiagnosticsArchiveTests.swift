import XCTest
@testable import CruiseMesh

final class DiagnosticsArchiveTests: XCTestCase {
    private var workDirectory: URL!

    override func setUpWithError() throws {
        workDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("diagnostics-archive-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: workDirectory, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: workDirectory)
        DiagnosticsArchive.deleteArchives()
    }

    private func file(_ name: String, _ contents: String) throws -> URL {
        let url = workDirectory.appendingPathComponent(name)
        try contents.write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    /// The whole point of the zip: every captured file has to be inside it.
    /// Sharing them as separate attachments let receiving apps keep the first
    /// and drop the rest, silently.
    ///
    /// Read as bytes rather than unzipped -- `Process` does not exist on iOS,
    /// and a zip records each entry's name in the clear, so finding all three
    /// names in a well-formed archive is the check that matters here.
    func testArchiveHoldsEveryCapturedFile() throws {
        let log = try file("cruisemesh-diagnostics.txt", "radio narrative\n")
        let crash = try file("diagnostic-2026-08-03.json", "{}\n")
        let csv = try file("cruisemesh-field-metrics.csv", "a,b\n1,2\n")

        let archive = try XCTUnwrap(
            DiagnosticsArchive.write(files: [log, crash, csv], name: "cruisemesh-diagnostics-2026-08-03")
        )
        XCTAssertEqual(archive.lastPathComponent, "cruisemesh-diagnostics-2026-08-03.zip")

        let data = try Data(contentsOf: archive)
        XCTAssertEqual(Array(data.prefix(4)), [0x50, 0x4B, 0x03, 0x04], "not a zip")
        let bytes = String(decoding: data, as: UTF8.self)
        for expected in [
            "cruisemesh-diagnostics.txt",
            "diagnostic-2026-08-03.json",
            "cruisemesh-field-metrics.csv",
        ] {
            XCTAssertTrue(bytes.contains(expected), "\(expected) missing from the archive")
        }
    }

    func testNothingCapturedProducesNoArchive() {
        XCTAssertNil(DiagnosticsArchive.write(files: [], name: "cruisemesh-diagnostics-2026-08-03"))
    }

    /// A share whose files vanished under it must report failure rather than
    /// hand the sheet an empty zip.
    func testMissingSourcesProduceNoArchive() {
        let missing = workDirectory.appendingPathComponent("never-written.csv")
        XCTAssertNil(DiagnosticsArchive.write(files: [missing], name: "cruisemesh-diagnostics-2026-08-03"))
    }

    /// "Delete captured diagnostics" has to take the zip too, or it leaves a
    /// full second copy of everything it claimed to erase.
    func testDeleteRemovesWrittenArchives() throws {
        let log = try file("cruisemesh-diagnostics.txt", "narrative\n")
        let archive = try XCTUnwrap(
            DiagnosticsArchive.write(files: [log], name: "cruisemesh-diagnostics-2026-08-03")
        )
        XCTAssertTrue(FileManager.default.fileExists(atPath: archive.path))

        DiagnosticsArchive.deleteArchives()

        XCTAssertFalse(FileManager.default.fileExists(atPath: archive.path))
    }

    /// The date is what makes a forwarded archive attributable, so the name
    /// has to carry one in a fixed, sortable format regardless of locale.
    func testTodaysNameCarriesAnIsoDate() {
        let name = DiagnosticsArchive.todaysName()
        XCTAssertNotNil(
            name.range(of: "^cruisemesh-diagnostics-\\d{4}-\\d{2}-\\d{2}$", options: .regularExpression),
            "unexpected archive name: \(name)"
        )
    }
}
