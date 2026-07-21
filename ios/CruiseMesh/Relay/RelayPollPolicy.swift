import Foundation

/// Pure relay-poll cadence policy (battery, 2026-07-21) -- mirrors Android's
/// `RadioPowerPolicy.relayPollIntervalMs` (`RadioPowerPolicy.kt`, "Relay poll
/// cadence" section) case-for-case, with one iOS-specific addition.
///
/// `RelayPushClient`'s class doc notes the 60s poll (`MeshController.relayTimer`
/// / `runRelaySync`) is the *only* relay-delivery path that survives
/// backgrounding on iOS -- there is no persistent background execution
/// context here the way Android has a foreground service, so the WS push
/// socket does not reliably stay open once the app is suspended. That makes
/// the backoff below strictly narrower than Android's: it only ever applies
/// while the app is foregrounded *and* push is healthy. Backgrounding
/// short-circuits straight back to the fast safety-net cadence regardless of
/// push health, since the poll needs to already be at its correctness-critical
/// cadence the instant background execution windows (kept alive by
/// CoreBluetooth activity) are all that's left.
enum RelayPollPolicy {
    /// Safety-net relay-poll cadence while `RelayPushClient`'s WS push is
    /// healthy and the app is foregrounded. Mirrors Android's
    /// `RELAY_POLL_HEALTHY_MS`.
    static let healthyForegroundMs: Int64 = 900_000

    /// Relay-poll cadence while push is unhealthy/has never connected, or the
    /// app is backgrounded. This is the original fixed interval this policy
    /// replaces. Mirrors Android's `RELAY_POLL_UNHEALTHY_MS`.
    static let unhealthyOrBackgroundMs: Int64 = 60_000

    /// One-shot reschedule delay right after a healthy-to-down push
    /// transition while foregrounded, so a missed push during the dying
    /// socket is still caught quickly. Mirrors Android's
    /// `RELAY_POLL_TRANSITION_MS`.
    static let transitionMs: Int64 = 5_000

    /// Next relay-poll interval given whether push was healthy at the last
    /// decision (`previouslyHealthy`, `nil` before any decision has been made
    /// yet), whether it's healthy now (`currentlyHealthy`), and whether the
    /// app is currently foregrounded (`foreground`).
    ///
    /// Backgrounding always returns `unhealthyOrBackgroundMs`, independent of
    /// the health pair -- see the type doc. Otherwise this is exactly
    /// Android's rule: the healthy-to-down transition gets one short
    /// interval, every other case uses the steady-state interval for the
    /// current health.
    static func relayPollIntervalMs(
        previouslyHealthy: Bool?,
        currentlyHealthy: Bool,
        foreground: Bool
    ) -> Int64 {
        guard foreground else { return unhealthyOrBackgroundMs }
        if previouslyHealthy == true, !currentlyHealthy { return transitionMs }
        return currentlyHealthy ? healthyForegroundMs : unhealthyOrBackgroundMs
    }
}
