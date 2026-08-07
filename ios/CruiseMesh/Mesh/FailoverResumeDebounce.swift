import Foundation

/// Thin shell wrapper over the core's per-peer failover-resume debounce (see
/// `CoreFailoverResumeDebounce` for the field bug, the window's sizing and the
/// coalescing rule). Android wraps the same core object, so the window cannot
/// drift between the platforms — same arrangement as `ReconnectBackoffTracker`.
///
/// Keys are the peer's UserID hex (`UserIdHex.encode`), never a link address:
/// the point is to coalesce the several *links* one logical peer loses in a
/// single radio event down to one resume.
final class FailoverResumeDebounce {
    static var defaultWindowMs: Int64 { coreFailoverResumeWindowMs() }

    private let core: CoreFailoverResumeDebounce

    init(windowMs: Int64 = FailoverResumeDebounce.defaultWindowMs) {
        core = CoreFailoverResumeDebounce.withWindowMs(windowMs: windowMs)
    }

    var windowMs: Int64 { core.windowMs() }

    /// Returns the delay to schedule the resume for, or `nil` when a window
    /// that is already armed for `key` will cover this failover too.
    func request(key: String, nowMs: Int64) -> Int64? { core.request(key: key, nowMs: nowMs) }

    /// The scheduled resume for `key` is running; the window is over.
    func fired(key: String) { core.fired(key: key) }

    func cancel(key: String) { core.cancel(key: key) }
    func isPending(key: String) -> Bool { core.isPending(key: key) }
    func clear() { core.clear() }
}
