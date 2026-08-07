import Foundation

/// Thin shell wrapper over the core's per-peer failover-resume debounce (see
/// `CoreFailoverResumeDebounce` for the field bug, the window's sizing and the
/// coalescing rule). Android wraps the same core object, so the window cannot
/// drift between the platforms — same arrangement as `ReconnectBackoffTracker`.
///
/// Keys are the peer's UserID hex (`UserIdHex.encode`), never a link address:
/// the point is to coalesce the several *links* one logical peer loses in a
/// single radio event down to one resume.
///
/// `nowMs` must come from `FailoverResumeDebounce.monotonicNowMs` — the same
/// clock `DispatchQueue.asyncAfter` counts down on. Measuring the window on the
/// wall clock while the timer runs on a monotonic one lets a clock correction
/// split one burst into two resumes.
final class FailoverResumeDebounce {
    static var defaultWindowMs: Int64 { coreFailoverResumeWindowMs() }

    /// Monotonic milliseconds, matching `DispatchTime`-based scheduling.
    static var monotonicNowMs: Int64 { Int64(DispatchTime.now().uptimeNanoseconds / 1_000_000) }

    private let core: CoreFailoverResumeDebounce

    init(windowMs: Int64 = FailoverResumeDebounce.defaultWindowMs) {
        core = CoreFailoverResumeDebounce.withWindowMs(windowMs: windowMs)
    }

    var windowMs: Int64 { core.windowMs() }

    /// Returns the delay to schedule the resume for plus the token to hand back
    /// to `fired`, or `nil` when a window that is already armed for `key` will
    /// cover this failover too.
    func request(key: String, nowMs: Int64) -> CoreFailoverResumeArm? {
        core.request(key: key, nowMs: nowMs)
    }

    /// The resume scheduled for `key` as `token` is running; that window is
    /// over. A token from a window that has since been replaced is ignored, so
    /// a timer landing just as a new window is armed cannot clear the new one.
    func fired(key: String, token: Int64) { core.fired(key: key, token: token) }

    func cancel(key: String) { core.cancel(key: key) }
    func isPending(key: String) -> Bool { core.isPending(key: key) }
    func clear() { core.clear() }
}
