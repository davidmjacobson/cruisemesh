import Foundation
import os.log

/// Live low-latency companion to the poll/fetch path (`MeshController.runRelaySync`
/// / `RelayClient`): opens relayd's `GET /ws` broadcast (relayd/src/lib.rs module
/// docs, "WebSocket push") for the caller's own relay config and, on every pushed
/// frame, invokes `onPush` -- nothing else. This is the iOS half of DTN_TODOS.md
/// D3; it mirrors Android's `RelayPushClient`
/// (`android/app/.../relay/RelayPushClient.kt`) using `URLSessionWebSocketTask`
/// instead of OkHttp. Read the Kotlin class doc for the fuller "why a doorbell,
/// not a second delivery path" reasoning -- decision §3.3 (DTN_TODOS.md) applies
/// unchanged: any pushed frame just means "go run the authoritative poll pass
/// now." This class never fetches, decodes, acks, or stores an envelope itself;
/// `MeshController.runRelaySync`'s cursor and ack logic remain the single source
/// of truth for delivery.
///
/// Because the doorbell and the authoritative fetch are decoupled, calling
/// `onPush` once per frame (rather than trying to interpret its contents -- this
/// class deliberately never parses the text payload) is fine: `onPush` is
/// expected to be `MeshController.runRelaySync`, whose in-flight/pending flags
/// already coalesce back-to-back calls into a single extra pass.
///
/// Reconnects with capped exponential backoff (`RelayPushBackoff`) whenever the
/// socket drops, whether from a server-side write-timeout/lag eviction, an
/// explicit close, or plain network loss. Hints are recomputed from
/// `hintsProvider` on every (re)connect -- not cached -- so a contact or group
/// added after the socket is already open is picked up on the next reconnect
/// without this class needing its own change tracking; until then the 60s poll
/// already covers it.
///
/// All mutable state is confined to `queue`, a private serial queue that plays
/// the same role Android's `mainHandler` does: it serializes `start`/`stop`/
/// reconnect scheduling and guards against acting on a stale socket's delegate
/// callbacks after a newer one has already replaced it (checked via reference
/// identity against `webSocketTask`).
///
/// ### iOS execution-time limitation
///
/// Unlike Android's foreground service, this app has no persistent background
/// execution context. iOS grants CPU/network time to a WebSocket only while the
/// app is in the foreground (this socket does not ride either of the
/// `bluetooth-central`/`bluetooth-peripheral` background modes the mesh's BLE
/// transport uses); when the app is suspended, iOS tears the connection down
/// without necessarily running any delegate callback here to say so. There is no
/// way to keep this doorbell alive in the background, so `MeshController`'s poll
/// (`runRelaySync`, driven by `relayTimer`) is not just a fallback for a
/// dropped socket -- it is the *only* relay-delivery path that survives
/// backgrounding at all. This is exactly why decision §3.3 requires the push
/// path to carry no state of its own: losing it silently degrades latency, never
/// correctness. Battery, 2026-07-21: it's also exactly why `RelayPollPolicy`
/// pins the poll back to its fast 60s cadence the moment the app backgrounds,
/// regardless of this socket's health -- see that type's doc.
///
/// ### Network binding
///
/// Unlike Android, which pins relay HTTP traffic to a specific
/// `ConnectivityManager`-granted network (`MeshService.relayBindTarget()`) to
/// work around an associated-but-dead Wi-Fi outranking a validated cellular
/// network, iOS's `RelayClient` and this socket both ride whatever path the
/// system currently prefers -- there is no analogous per-socket network pinning
/// here, and `NWPathMonitor`-driven path selection has not exhibited that
/// Android-specific failure mode. Nothing to mirror on this axis.
///
/// ### Health signal (battery, 2026-07-21)
///
/// `isHealthy()` and the `onHealthChanged` callback exist purely so
/// `MeshController` can back `relayTimer`'s poll cadence off
/// (`RelayPollPolicy.relayPollIntervalMs`) while this socket is doing the
/// low-latency job instead: `onHealthChanged` fires `true` on a real open
/// (`didOpenWithProtocol`) and `false` on a real drop (`handleDisconnectLocked`
/// -- receive failure, server close, or task completion error), but
/// deliberately *not* from `stop()` -- an intentional teardown (mesh
/// stopping, or `start` reconfiguring to a new `RelayConfig`) isn't the
/// "something went wrong, catch up sooner" signal the transition exists for.
/// Mirrors Android's `RelayPushClient.kt` exactly (same class-doc section,
/// same rationale).
///
/// `isHealthy()` is read synchronously from `MeshController`'s relay-poll
/// tick, on that controller's own serial mesh queue, unlike the rest of this
/// class's state which is confined to `queue` -- so it's backed by its own
/// lock rather than round-tripping through `queue.async`.
final class RelayPushClient: NSObject {
    private static let connectTimeout: TimeInterval = 10
    private static let log = Logger(subsystem: "com.cruisemesh", category: "RelayPushClient")

    private let backoff = RelayPushBackoff()
    private let queue = DispatchQueue(label: "com.cruisemesh.relaypush")
    private let onPush: () -> Void
    private let onHealthChanged: (Bool) -> Void

    private var urlSession: URLSession?
    private var webSocketTask: URLSessionWebSocketTask?
    private var desiredConfig: RelayConfig?
    private var hintsProvider: (() -> RelayPushSubscription)?
    private var stopped = true
    private var reconnectWorkItem: DispatchWorkItem?

    private let healthLock = NSLock()
    private var healthyLocked = false

    init(onPush: @escaping () -> Void, onHealthChanged: @escaping (Bool) -> Void = { _ in }) {
        self.onPush = onPush
        self.onHealthChanged = onHealthChanged
        super.init()
    }

    /// Whether the push socket is currently open -- see the class doc's
    /// "Health signal" section. Thread-safe; safe to call from any queue.
    func isHealthy() -> Bool {
        healthLock.lock()
        defer { healthLock.unlock() }
        return healthyLocked
    }

    private func setHealthy(_ healthy: Bool) {
        healthLock.lock()
        healthyLocked = healthy
        healthLock.unlock()
    }

    /// (Re)starts the push subscription against `config`, computing the
    /// subscribed `hints=` set from `hintsProvider` on every (re)connect. Safe
    /// to call repeatedly (e.g. from every path-update callback); a no-op if
    /// already started against an equal `config`.
    func start(config: RelayConfig, hintsProvider: @escaping () -> RelayPushSubscription) {
        queue.async { [self] in
            if !stopped, desiredConfig == config { return }
            stopLocked()
            stopped = false
            desiredConfig = config
            self.hintsProvider = hintsProvider
            backoff.recordSuccess() // fresh target: start its backoff from the floor
            connectLocked()
        }
    }

    /// Reopens the socket, if this client is currently subscribed to `config`,
    /// so a changed subscribe cursor reaches relayd.
    ///
    /// relayd's push gate is the `after=` the client subscribed with: it only
    /// pushes rows above that id, and it only ever moves that id *up* for the
    /// life of the socket (`handle_ws` keeps a local `after` and runs
    /// `after = after.max(id)` on every row it replays or pushes). So a socket
    /// can never deliver a row at or below the value it was opened with, and
    /// when the poll path lowers this mailbox's frontier -- a completed sweep
    /// proving the relay's row ids restarted underneath it, see
    /// `relayFrontierAfterCompletedSweep` in the core -- a socket opened
    /// against the old frontier stays deaf to the entire rebuilt mailbox until
    /// something unrelated happens to drop it. On a phone that can be hours.
    ///
    /// A no-op for any other relay (the sync pass polls every contact's
    /// mailbox, and only one of them is the one this socket watches), and a
    /// no-op when stopped. Deliberately reports the drop through
    /// `onHealthChanged` even though it is intentional -- unlike `stop`, this
    /// client is still trying to keep a socket up, and the poll cadence should
    /// cover the gap until the new one opens. Mirrors RelayPushClient.kt.
    func resubscribe(config: RelayConfig) {
        queue.async { [self] in
            guard !stopped, desiredConfig == config, hintsProvider != nil else { return }
            Self.log.info("Reopening the relay push socket to pick up a changed cursor")
            reconnectWorkItem?.cancel()
            reconnectWorkItem = nil
            // Every delegate callback and the receive loop check
            // `webSocketTask === task`, so clearing this first is what orphans
            // the old socket's late callbacks rather than letting them tear
            // down its successor.
            webSocketTask?.cancel(with: .goingAway, reason: nil)
            webSocketTask = nil
            urlSession?.invalidateAndCancel()
            urlSession = nil
            let wasHealthy = isHealthy()
            setHealthy(false)
            if wasHealthy { onHealthChanged(false) }
            // Not a connection failure: a deliberate reopen must not inherit
            // the backoff of whatever went wrong last.
            backoff.recordSuccess()
            connectLocked()
        }
    }

    /// Closes the socket (if any) and cancels any pending reconnect. Idempotent.
    func stop() {
        queue.async { [self] in stopLocked() }
    }

    private func stopLocked() {
        stopped = true
        desiredConfig = nil
        hintsProvider = nil
        reconnectWorkItem?.cancel()
        reconnectWorkItem = nil
        webSocketTask?.cancel(with: .goingAway, reason: nil)
        webSocketTask = nil
        urlSession?.invalidateAndCancel()
        urlSession = nil
        // No onHealthChanged callback here -- see the class doc's "Health
        // signal" section: an intentional stop is not the transition the
        // callback exists to catch.
        setHealthy(false)
    }

    private func connectLocked() {
        guard !stopped, let config = desiredConfig else { return }
        let subscription = hintsProvider?() ?? RelayPushSubscription(hints: [], afterId: 0)
        let hints = subscription.hints
        guard !hints.isEmpty else {
            // Nothing addressed to us yet (e.g. no contacts/groups). relayd
            // rejects a hint-less subscribe with 400; just retry once hints
            // might exist rather than treating this as a connection failure.
            scheduleReconnectLocked()
            return
        }
        guard let url = Self.buildWebSocketURL(
            config: config,
            hints: hints,
            afterId: subscription.afterId
        ) else {
            Self.log.warning("Failed to build relay push URL")
            scheduleReconnectLocked()
            return
        }
        var request = URLRequest(url: url, timeoutInterval: Self.connectTimeout)
        request.setValue("Bearer \(config.relayToken)", forHTTPHeaderField: "Authorization")
        let session = URLSession(configuration: .ephemeral, delegate: self, delegateQueue: nil)
        urlSession = session
        let task = session.webSocketTask(with: request)
        webSocketTask = task
        Self.log.info("Connecting relay push socket")
        task.resume()
        receiveNext(on: task)
    }

    private func receiveNext(on task: URLSessionWebSocketTask) {
        task.receive { [weak self] result in
            guard let self else { return }
            self.queue.async {
                // A stale task's completion arriving after it was replaced or
                // torn down -- ignore it rather than disturbing current state.
                guard self.webSocketTask === task, !self.stopped else { return }
                switch result {
                case .success:
                    // Content is ignored on purpose -- see class doc. Any
                    // pushed frame, replayed or live, just means "go run the
                    // authoritative poll pass now."
                    self.onPush()
                    self.receiveNext(on: task)
                case .failure(let error):
                    Self.log.warning("Relay push socket receive failed: \(error.localizedDescription, privacy: .public)")
                    self.handleDisconnectLocked()
                }
            }
        }
    }

    private func handleDisconnectLocked() {
        guard !stopped else { return }
        webSocketTask = nil
        urlSession?.invalidateAndCancel()
        urlSession = nil
        let wasHealthy = isHealthy()
        setHealthy(false)
        if wasHealthy { onHealthChanged(false) }
        backoff.recordFailure()
        scheduleReconnectLocked()
    }

    private func scheduleReconnectLocked() {
        guard !stopped else { return }
        reconnectWorkItem?.cancel()
        let item = DispatchWorkItem { [weak self] in self?.connectLocked() }
        reconnectWorkItem = item
        queue.asyncAfter(deadline: .now() + .milliseconds(Int(backoff.nextDelayMs())), execute: item)
    }

    /// `afterId` used to be hardcoded to 0, which asked relayd to replay the
    /// whole mailbox as push frames on every reconnect -- frames this class
    /// discards one by one, because it deliberately ignores frame content
    /// (see the class doc). Passing the poll path's persisted frontier makes
    /// a reconnect cost what it should: the mail that actually arrived since.
    /// Correctness is unchanged either way, since a frame is only a doorbell.
    ///
    /// A negative cursor is clamped to 0: relayd rejects a negative `after`,
    /// and failing the doorbell over a corrupt local value would be a worse
    /// trade than replaying.
    static func buildWebSocketURL(config: RelayConfig, hints: [Data], afterId: Int64) -> URL? {
        let encodedHints = hints.map(base64URLEncode).joined(separator: ",")
        // Empty means the core refused the URL (non-HTTPS). A bare
        // "/ws?..." is a relative URL the socket cannot open, so fail to
        // build here and let the caller log it and back off.
        let normalized = normalizeRelayUrl(config.relayUrl)
        guard !normalized.isEmpty else { return nil }
        let wsBase = toWebSocketScheme(normalized)
        return URL(string: "\(wsBase)/ws?hints=\(encodedHints)&after=\(max(0, afterId))")
    }

    private static func toWebSocketScheme(_ normalized: String) -> String {
        if normalized.hasPrefix("https://") {
            return "wss://" + normalized.dropFirst("https://".count)
        } else if normalized.hasPrefix("http://") {
            return "ws://" + normalized.dropFirst("http://".count)
        }
        return normalized
    }

    /// RFC 4648 base64url, no padding -- matches relayd/core's `BASE64URL_NOPAD`
    /// (`core/src/relay_wire.rs`), which is what `RelayClient`'s HTTP fetch path
    /// already sends via `relayBuildFetchPath`.
    private static func base64URLEncode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

extension RelayPushClient: URLSessionWebSocketDelegate {
    func urlSession(
        _ session: URLSession,
        webSocketTask: URLSessionWebSocketTask,
        didOpenWithProtocol _: String?
    ) {
        queue.async { [self] in
            guard self.webSocketTask === webSocketTask else { return }
            Self.log.info("Relay push socket open")
            backoff.recordSuccess()
            setHealthy(true)
            onHealthChanged(true)
        }
    }

    func urlSession(
        _ session: URLSession,
        webSocketTask: URLSessionWebSocketTask,
        didCloseWith closeCode: URLSessionWebSocketTask.CloseCode,
        reason: Data?
    ) {
        queue.async { [self] in
            guard self.webSocketTask === webSocketTask else { return }
            Self.log.info("Relay push socket closed: \(String(describing: closeCode), privacy: .public)")
            handleDisconnectLocked()
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let error else { return }
        queue.async { [self] in
            guard self.webSocketTask === task else { return }
            Self.log.warning("Relay push socket failed: \(error.localizedDescription, privacy: .public)")
            handleDisconnectLocked()
        }
    }
}
