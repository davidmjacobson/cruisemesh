import Foundation
import XCTest
@testable import CruiseMesh

/// The shell loop the §13 WP3 gate rests on, driven end to end in process.
///
/// The core already proves its two state machines against each other
/// (`core/tests/device_link_ceremony.rs`). What is unproven until here is the
/// Swift that will actually be holding them on two phones: that it answers the
/// one outstanding action, that it never confirms on the wrong device, and that
/// it behaves the same whether bytes arrive the instant they are sent or sit in a
/// mailbox until somebody polls.
///
/// Both transports the gate names are represented by their *shape*, which is the
/// only thing the ceremony can tell apart: `.live` is a LAN socket, and
/// `.mailbox` is a relay rendezvous, where every read costs a poll and the clock
/// really moves.
///
/// The Swift twin of Android's `LinkCeremonyDriverTest`.
final class LinkCeremonyDriverTests: XCTestCase {

    private enum Shape { case live, mailbox }

    /// A thread-safe FIFO with an optional bounded wait, so one queue can stand
    /// in for both transport shapes.
    private final class LinkPipe {
        private let condition = NSCondition()
        private var items: [Data] = []

        func put(_ bytes: Data) {
            condition.lock()
            items.append(bytes)
            condition.signal()
            condition.unlock()
        }

        /// Takes the head, waiting at most `waitMs`. `waitMs == 0` is a mailbox
        /// look: it never waits.
        func take(waitMs: Int64) -> Data? {
            condition.lock()
            defer { condition.unlock() }
            if items.isEmpty, waitMs > 0 {
                _ = condition.wait(until: Date().addingTimeInterval(Double(waitMs) / 1_000))
            }
            return items.isEmpty ? nil : items.removeFirst()
        }
    }

    /// One direction of an in-memory pipe, with the transport's shape baked in.
    private final class QueueWire: LinkWire {
        private let inbox: LinkPipe
        private let outbox: LinkPipe
        private let shape: Shape

        init(inbox: LinkPipe, outbox: LinkPipe, shape: Shape) {
            self.inbox = inbox
            self.outbox = outbox
            self.shape = shape
        }

        func send(_ bytes: Data) throws { outbox.put(bytes) }

        func receive(waitMs: Int64) throws -> Data? {
            switch shape {
            case .live:
                return inbox.take(waitMs: waitMs)
            // A mailbox does not hand anything over the moment it lands: the
            // reader looks, finds nothing, and comes back later.
            case .mailbox:
                return inbox.take(waitMs: 0)
            }
        }

        func close() {}
    }

    private struct Run {
        let newcomer: CoreLinkSummary
        let approver: CoreLinkSummary
        let newcomerSas: String?
        let approverSas: String?
        let newcomerWasAskedToConfirm: Bool
    }

    /// Small mutable box the two ceremony threads and the test share.
    private final class Recorder {
        private let lock = NSLock()
        private var newcomerSas: String?
        private var approverSas: String?
        private var newcomerAsked = false

        func recordNewcomer(sas: String, confirmHere: Bool) {
            lock.lock()
            newcomerSas = sas
            newcomerAsked = newcomerAsked || confirmHere
            lock.unlock()
        }

        func recordApprover(sas: String) {
            lock.lock()
            approverSas = sas
            lock.unlock()
        }

        var snapshot: (String?, String?, Bool) {
            lock.lock()
            defer { lock.unlock() }
            return (newcomerSas, approverSas, newcomerAsked)
        }

        var approverHasSas: Bool {
            lock.lock()
            defer { lock.unlock() }
            return approverSas != nil
        }
    }

    private func budgets() -> LinkBudgets {
        LinkBudgets(deadlineMs: 20_000, pollIntervalMs: 20, qrLifetimeMs: 60_000)
    }

    private func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1_000) }

    private func briefSleep(_ ms: Int64) {
        guard ms > 0 else { return }
        Thread.sleep(forTimeInterval: Double(min(ms, 50)) / 1_000)
    }

    /// Drive both halves to an ending, each on its own thread — which is how they
    /// will really run, one per phone.
    private func link(
        shape: Shape,
        activeDeviceCount: UInt32 = 1,
        answer: LinkConfirmDecision = .matches
    ) throws -> Run {
        let toNewcomer = LinkPipe()
        let toApprover = LinkPipe()
        let newcomer = try CoreLinkNewDevice(
            lanEndpoints: ["192.168.1.24:45892"],
            relayBaseUrls: [],
            nowMs: nowMs(),
            budgets: budgets()
        )
        let approver = try CoreLinkApprovingDevice.scan(
            qrText: newcomer.qrText(),
            activeDeviceCount: activeDeviceCount,
            budgets: budgets()
        )
        let recorder = Recorder()

        let newcomerDriver = LinkCeremonyDriver(
            machine: NewDeviceMachine(newcomer),
            wire: QueueWire(inbox: toNewcomer, outbox: toApprover, shape: shape),
            clock: nowMs,
            sleep: briefSleep,
            observer: LinkCeremonyObserver(
                onSas: { sas, confirmHere, _ in
                    recorder.recordNewcomer(sas: sas, confirmHere: confirmHere)
                }
            )
        )
        let approverDriver = LinkCeremonyDriver(
            machine: ApprovingDeviceMachine(approver),
            wire: QueueWire(inbox: toApprover, outbox: toNewcomer, shape: shape),
            clock: nowMs,
            sleep: briefSleep,
            observer: LinkCeremonyObserver(
                onSas: { sas, _, _ in recorder.recordApprover(sas: sas) }
            ),
            confirmDecision: { recorder.approverHasSas ? answer : .waiting }
        )

        let box = SummaryBox()
        let newcomerDone = expectation(description: "new device finished")
        let approverDone = expectation(description: "approving device finished")
        Thread.detachNewThread {
            box.setNewcomer(try? newcomerDriver.run())
            newcomerDone.fulfill()
        }
        Thread.detachNewThread {
            box.setApprover(try? approverDriver.run())
            approverDone.fulfill()
        }
        wait(for: [newcomerDone, approverDone], timeout: 30)

        let (newcomerSas, approverSas, asked) = recorder.snapshot
        return Run(
            newcomer: try XCTUnwrap(box.newcomer, "the new device never finished"),
            approver: try XCTUnwrap(box.approver, "the approving device never finished"),
            newcomerSas: newcomerSas,
            approverSas: approverSas,
            newcomerWasAskedToConfirm: asked
        )
    }

    private final class SummaryBox {
        private let lock = NSLock()
        private var newcomerSummary: CoreLinkSummary?
        private var approverSummary: CoreLinkSummary?

        func setNewcomer(_ summary: CoreLinkSummary?) {
            lock.lock(); newcomerSummary = summary; lock.unlock()
        }

        func setApprover(_ summary: CoreLinkSummary?) {
            lock.lock(); approverSummary = summary; lock.unlock()
        }

        var newcomer: CoreLinkSummary? {
            lock.lock(); defer { lock.unlock() }
            return newcomerSummary
        }

        var approver: CoreLinkSummary? {
            lock.lock(); defer { lock.unlock() }
            return approverSummary
        }
    }

    /// **The gate's LAN shape**, driven by the loop that will drive the phones.
    func testTwoDevicesLinkOverALiveLink() throws {
        assertLinked(try link(shape: .live))
    }

    /// **The gate's relay-only shape.** Nothing arrives the moment it is sent;
    /// each side polls, finds an empty mailbox, and ticks. The ceremony cannot
    /// tell, which is the property under test.
    func testTwoDevicesLinkOverAStoreAndForwardRendezvous() throws {
        assertLinked(try link(shape: .mailbox))
    }

    private func assertLinked(_ run: Run) {
        XCTAssertEqual(run.newcomer.outcome, .channelReady)
        XCTAssertEqual(run.approver.outcome, .channelReady)

        // The digits a person compares are the same on both screens, and the
        // channel both ends hold is the same channel.
        XCTAssertNotNil(run.newcomerSas)
        XCTAssertEqual(run.newcomerSas, run.approverSas)
        XCTAssertEqual(run.newcomer.sas, run.approver.sas)
        XCTAssertEqual(run.newcomer.channelBinding, run.approver.channelBinding)

        // §9.2: the confirm lives on the device that is already part of the
        // person. The new phone is never offered it, on any transport.
        XCTAssertFalse(run.newcomerWasAskedToConfirm, "the new device was offered the confirm")

        // Nothing was answered out of turn: a resume that did not match the
        // outstanding action would have been counted here instead of acted on.
        XCTAssertEqual(run.newcomer.staleResumesIgnored, 0)
        XCTAssertEqual(run.approver.staleResumesIgnored, 0)
    }

    /// The digits did not match, so nothing is adopted and both sides say why —
    /// including the phone that had no button, which learns it from the channel
    /// rather than from a timeout.
    func testDigitsThatDoNotMatchEndTheCeremonyOnBothSides() throws {
        let run = try link(shape: .live, answer: .doesNotMatch)
        XCTAssertEqual(run.approver.outcome, .declined)
        XCTAssertEqual(run.newcomer.outcome, .declined)
    }

    /// §14.3's hard cap, felt at the only moment it can be: the scan. The scanner
    /// refuses before a single byte is sent, so the phone showing the QR is left
    /// waiting and learns nothing about the other person's device count.
    func testAPersonAtTheHardCapLinksNothing() throws {
        let newcomer = try CoreLinkNewDevice(
            lanEndpoints: ["192.168.1.24:45892"],
            relayBaseUrls: [],
            nowMs: nowMs(),
            budgets: budgets()
        )
        let approver = try CoreLinkApprovingDevice.scan(
            qrText: newcomer.qrText(),
            activeDeviceCount: 16,
            budgets: budgets()
        )
        let summary = try LinkCeremonyDriver(
            machine: ApprovingDeviceMachine(approver),
            wire: QueueWire(inbox: LinkPipe(), outbox: LinkPipe(), shape: .live),
            clock: nowMs,
            sleep: briefSleep
        ).run()
        XCTAssertEqual(summary.outcome, .deviceCapReached)
        XCTAssertEqual(summary.messagesSent, 0)
        XCTAssertNil(newcomer.summary())
    }

    /// §9.2's one forbidden branch, asserted rather than commented: a build in
    /// which the new phone can confirm its own link is a build that fails here.
    func testTheNewDeviceCannotConfirmItsOwnLink() throws {
        let newcomer = try CoreLinkNewDevice(
            lanEndpoints: [],
            relayBaseUrls: [],
            nowMs: nowMs(),
            budgets: budgets()
        )
        let machine = NewDeviceMachine(newcomer)
        XCTAssertThrowsError(try machine.confirmHere(nowMs: nowMs(), matches: true))
    }
}
