import XCTest
@testable import CruiseMesh

/// The archive is what leaves the phone, so the property worth pinning here is
/// that the sink it is written through redacts -- not that the core scanner
/// works, which `core/src/log_redaction.rs` owns and tests in far more detail.
final class DiagnosticLogRedactionTests: XCTestCase {
    private let salt = "0123456789abcdef0123456789abcdef"

    func testWiFiAddressIsReplacedAndThePortSurvives() {
        let redacted = DiagnosticLogExport.redactLines(
            salt: salt,
            text: "LAN session ready on 192.168.1.42:7777"
        )
        XCTAssertFalse(redacted.contains("192.168.1.42"), redacted)
        XCTAssertTrue(redacted.hasSuffix(":7777"), redacted)
    }

    func testContactIdIsReplaced() {
        let id = String(repeating: "9f", count: 32)
        let redacted = DiagnosticLogExport.redactLines(salt: salt, text: "HELLO from \(id)")
        XCTAssertFalse(redacted.contains(id), redacted)
        XCTAssertTrue(redacted.hasPrefix("HELLO from id-"), redacted)
    }

    func testTheSamePeerReadsAsTheSamePeerAcrossLines() {
        let first = DiagnosticLogExport.redactLines(salt: salt, text: "connected 10.0.0.7")
        let second = DiagnosticLogExport.redactLines(salt: salt, text: "closed 10.0.0.7")
        XCTAssertEqual(
            first.replacingOccurrences(of: "connected ", with: ""),
            second.replacingOccurrences(of: "closed ", with: "")
        )
    }

    func testALineWithNothingSensitiveIsUntouched() {
        let line = "2026-08-27T14:23:11Z [RelaySyncEngine] I Relay sync complete: configs=2 in 1234ms"
        XCTAssertEqual(DiagnosticLogExport.redactLines(salt: salt, text: line), line)
    }

    /// A `composedMessage` can span lines, and so can the launch banner. Every
    /// line has to be scanned, and the shape has to survive.
    func testEveryLineOfAMultiLineBlockIsScanned() {
        let redacted = DiagnosticLogExport.redactLines(
            salt: salt,
            text: "first 10.0.0.7\nsecond AA:BB:CC:DD:EE:FF\nthird nothing here"
        )
        let lines = redacted.split(separator: "\n", omittingEmptySubsequences: false)
        XCTAssertEqual(lines.count, 3)
        XCTAssertFalse(redacted.contains("10.0.0.7"), redacted)
        XCTAssertFalse(redacted.contains("AA:BB"), redacted)
        XCTAssertEqual(String(lines[2]), "third nothing here")
    }
}
