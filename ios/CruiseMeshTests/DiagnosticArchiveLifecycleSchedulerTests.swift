import Foundation
import XCTest
@testable import CruiseMesh

final class DiagnosticArchiveLifecycleSchedulerTests: XCTestCase {
    func testScheduleReturnsWithoutWaitingForArchiveWorkAndCoalesces() {
        let workerStarted = expectation(description: "worker started")
        let workerFinished = expectation(description: "worker finished")
        let releaseWorker = DispatchSemaphore(value: 0)
        let scheduler = DiagnosticArchiveLifecycleScheduler(
            queue: DispatchQueue(label: "DiagnosticArchiveLifecycleSchedulerTests.blocked")
        )

        let scheduled = scheduler.schedule(
            beginBackgroundTask: { _ in {} },
            work: { _ in
                workerStarted.fulfill()
                releaseWorker.wait()
                workerFinished.fulfill()
            }
        )

        XCTAssertTrue(scheduled)
        wait(for: [workerStarted], timeout: 1)
        XCTAssertFalse(
            scheduler.schedule(
                beginBackgroundTask: { _ in {} },
                work: { _ in XCTFail("a coalesced archive must not run") }
            )
        )

        releaseWorker.signal()
        wait(for: [workerFinished], timeout: 1)
    }

    func testBackgroundTaskExpiryCancelsArchiveWork() {
        let workerStarted = expectation(description: "worker started")
        let workerCancelled = expectation(description: "worker observed cancellation")
        let backgroundTaskEnded = expectation(description: "background task ended")
        let scheduler = DiagnosticArchiveLifecycleScheduler(
            queue: DispatchQueue(label: "DiagnosticArchiveLifecycleSchedulerTests.expiry")
        )
        let expirationLock = NSLock()
        var expiration: (() -> Void)?

        XCTAssertTrue(
            scheduler.schedule(
                beginBackgroundTask: { handler in
                    expirationLock.lock()
                    expiration = handler
                    expirationLock.unlock()
                    return { backgroundTaskEnded.fulfill() }
                },
                work: { cancellation in
                    workerStarted.fulfill()
                    while !cancellation.isCancelled {
                        Thread.sleep(forTimeInterval: 0.001)
                    }
                    workerCancelled.fulfill()
                }
            )
        )

        wait(for: [workerStarted], timeout: 1)
        expirationLock.lock()
        let expire = expiration
        expirationLock.unlock()
        XCTAssertNotNil(expire)
        expire?()
        wait(for: [workerCancelled, backgroundTaskEnded], timeout: 1)
    }
}
