import AVFoundation
import Combine
import Foundation
import Network
import os.log

/**
 Owns BLE dual-role + frame handling + relay sync (Android `MeshService` parity).

 # Threading

 Every mesh event -- BLE and LAN frames, connects, disconnects, HELLO, the
 digest-maintenance and LAN-health ticks, the relay poll's bookkeeping, and
 the app's own start/stop/foreground calls -- runs on `meshQueue`, one private
 **serial** `DispatchQueue`. Nothing in this class runs on the main thread any
 more, and every stored property below is owned by that queue.

 This used to be a `@MainActor` type, so the whole inbound pipeline ran on the
 main thread: per frame, a failed pairwise unseal (X25519 + AEAD over payloads
 up to ~200 KiB), group-key open attempts, and a SQLite carry insert with
 budget enforcement. A field report has a phone on a BLE-only link taking a
 sustained multi-megabyte mule spray from a loaded courier: the UI stopped
 responding for as long as the spray lasted, and the app was then killed on a
 scene transition with the main thread still pegged. Android has always run
 the equivalent work on binder/store threads; this is the iOS half of that.

 ## Why a serial queue, and not an actor

 Two documented invariants need the mesh's events to be processed strictly one
 at a time, in arrival order:

 - **FI6**: a connect and a disconnect for the same address must be handled in
   the order the transport reported them, or a fast connect->disconnect can
   re-register a dead route.
 - **DTN D4** (see `processInboundEnvelope`): the seen-set is checked with a
   non-mutating `contains` and recorded only at a terminal handled state, and
   `relayForeign` re-floods before that record happens. Both are safe only
   because one envelope is processed to completion before the next one starts.

 A serial `DispatchQueue` gives exactly that and nothing subtler: `async`
 enqueues FIFO from any thread, and a block runs to completion before the next
 one starts -- there is no suspension point at which a second block could
 interleave. A Swift `actor` would not: every `await` inside an actor method
 is a re-entrancy point, so a single stray `await` added to the pipeline later
 would silently break both invariants with no compiler complaint. Nothing on
 the inbound path awaits today, and the queue means nothing can.

 ## What still runs on the main actor

 UI-facing state only, handed over with `onMain` (which is
 `DispatchQueue.main.async`, so it preserves the order the pipeline produced
 it in): `MeshConnectivityStatus`, `MeshRuntimeStatus`, and the Combine
 subjects in `ChatEvents.swift`, which hop themselves. Everything else the
 pipeline touches is safe off the main thread on its own terms -- the Rust
 core's store/seen-set/trackers are `Mutex`-backed, `MeshRouter`,
 `ChatVisibility`, `ContactRelaySilence` and `RelaySweepSession` are
 `NSLock`-backed, `LanCapabilityStore`/`ProfileStore` are `UserDefaults`,
 `LanEndpointCache` is `UserDefaults` plus its own `NSLock` (its save reads
 the stored entry before rewriting it, and per-operation atomicity is not
 enough for that), `LanTransportDiagnostics` publishes on main internally, and
 `BleTransport`/`LanTransport` each own a private queue.

 The relay sync *pass* itself is unchanged: it still runs in a detached task
 off any queue, still serialises through `relaySyncInFlight`, and still owns
 `ContactRelaySilence` alone -- the inbound pipeline never touches it, so the
 pass's assumption that its silence bookkeeping sees non-overlapping passes
 still holds.
 */
final class MeshController: ObservableObject, @unchecked Sendable {
    static let shared = MeshController()

    /// The single serial context every mesh event runs on. See the class doc
    /// for why it is a queue rather than an actor.
    private let meshQueue = DispatchQueue(label: "com.cruisemesh.mesh", qos: .userInitiated)

    private let log = Logger(subsystem: "com.cruisemesh", category: "MeshController")
    private let transport = BleTransport()
    /// Set while `applyLinkVisibility` has taken the mesh down for a §9.4
    /// pre-activation window, so the allow branch restarts only a mesh this
    /// ceremony stopped.
    private var linkSilenced = false
    private var lanTransport: LanTransport?
    private let lanHealth = LanHealthTracker()
    /// §10 step 5's re-offer bookkeeping; see `OwnRosterNoticeSchedule`.
    private let ownRosterNoticeSchedule = OwnRosterNoticeSchedule()
    /// §10 step 5's sweep motive; see `OwnDeviceSearchWindow`.
    private let ownDeviceSearchWindow = OwnDeviceSearchWindow()
    private let store = AppStore.get()

    /// The core encounter planner's driver, used only when
    /// `MeetEngineSettings` selects `.core`. Built against the process-wide
    /// route state, spray policy and carried-offer gate, because the planner
    /// has to record its cadence windows and walk cursors on the very objects
    /// the rest of this controller reads.
    private lazy var coreMeet = MeetAdapter(
        store: store,
        router: MeshRouter.coreState,
        spray: SprayPolicy.coreState,
        offers: CarriedOfferEpochGate.coreState,
        send: { address, frame in
            _ = MeshRouter.sendToAddress(address: address, frame: frame)
        }
    )
    private let incomingAnnouncements = IncomingMessageAnnouncementGate(
        announcer: LocalNotificationAnnouncer()
    )
    private let bluetoothAudioBackoff = BluetoothAudioBackoff()
    /// Coalesces the failover resume fan-out per logical peer — see
    /// `scheduleFailoverResume`. Confined to `meshQueue` like the rest of the
    /// mesh event state.
    private let failoverResumeDebounce = FailoverResumeDebounce()
    private var identity: Identity!
    /// Group digests this process has actually answered, keyed
    /// `peerHex/groupHex`. The 1:1 fallback still resends any shared group
    /// that is not in this set. Insert only after the spray gate allows.
    private var groupDigestAnswers = Set<String>()
    private var relayTimer: DispatchSourceTimer?
    /// CP2b: epoch ms until which relayd asked us not to sync again
    /// (`Retry-After` on a 429); 0 = no backoff. `runRelaySync` drops nudges
    /// inside the window; `relayRateLimitRetryWorkItem` retries at its end.
    private var relayRateLimitedUntilMs: Int64 = 0
    private var relayRateLimitRetryWorkItem: DispatchWorkItem?
    /// Pending resumption of a relay mailbox walk that hit its per-pass budget
    /// (`relayMailboxWalkAction`). At most one is scheduled at a time, whatever
    /// number of mailboxes yielded; each fires a whole sync pass, which picks
    /// every yielded mailbox up from its persisted cursor. Mirrors Android's
    /// `mailboxContinuationRunnable`.
    private var relayMailboxContinuationWorkItem: DispatchWorkItem?
    private let familyRelayRequestPacer = FamilyRelayRequestPacer()
    private let familyRelayBackoff = FamilyRelayBackoff()
    private var pathMonitor: NWPathMonitor?
    /// DTN_TODOS.md D3 (iOS half of audit finding F1, "relay poll-only"): opens
    /// relayd's `GET /ws` push socket (see `RelayPushClient`'s class doc) once
    /// the mesh is running, an identity/relay config exist, and the network
    /// path is satisfied, and calls `runRelaySync()` on every pushed frame
    /// instead of waiting for the next `relayTimer` tick. Never processes
    /// envelope content itself -- see `RelayPushClient`'s class doc. Mirrors
    /// Android's `relayPushClient` / `updateRelayPushSubscription`
    /// (`MeshService.kt`).
    ///
    /// Battery, 2026-07-21: also reports its connection health via
    /// `onRelayPushHealthChanged`, which `reschedulePoll` (through
    /// `RelayPollPolicy.relayPollIntervalMs`) uses to slow `relayTimer` down
    /// to a safety net while push is healthy and the app is foregrounded.
    private lazy var relayPushClient = RelayPushClient(
        onPush: { [weak self] in
            guard let self else { return }
            self.meshQueue.async { self.runRelaySync() }
        },
        onHealthChanged: { [weak self] healthy in
            guard let self else { return }
            self.meshQueue.async { self.onRelayPushHealthChanged(healthy) }
        }
    )
    private var isRunning = false
    private var meshRolesRunning = false
    private var bluetoothAudioConnected = false
    private var relayCancellable: AnyCancellable?
    /// Health `relayPushClient` reported at the last poll-interval decision;
    /// `nil` before the first one. See `onRelayPushHealthChanged`/
    /// `reschedulePoll`. Mirrors Android's `MeshService.lastKnownPushHealthy`.
    private var lastKnownPushHealthy: Bool?
    private var lanHealthTimer: DispatchSourceTimer?
    // D8: periodic re-digest bookkeeping.
    private var digestMaintenanceTimer: DispatchSourceTimer?
    // The map that used to live here — `lastDigestAtByAddress` — is gone. It
    // was written by every spray and read by only one of them (the maintenance
    // tick), so the event-driven call sites sprayed unconditionally; see issue
    // #280 and `core/src/spray_policy.rs`. Cadence, budgets, identical-set
    // suppression and receipt-quiet backoff are now one core decision,
    // consulted through `SprayPolicy`.
    /// The one pending `rearmGatedSpray` deferral per logical peer (keyed by
    /// hex user id), so a reconnect storm produces one resume rather than a
    /// timer per denial. Android's `pendingSprayDeferrals` is the twin.
    private var pendingSprayDeferrals: [String: DispatchWorkItem] = [:]
    /// A peer DIGEST the cadence gate refused, held (keyed by hex user id)
    /// until `resumeLogicalPeerSync` can answer it. See `GatedDigest`.
    private var gatedDigests: [String: GatedDigest] = [:]
    private var audioRouteObserver: NSObjectProtocol?
    private var relaySyncInFlight = false
    private var relaySyncPending = false
    /// APNs content-available wakes hold their background fetch completion
    /// until the authoritative relay pass finishes (or a 25-second safety
    /// deadline wins), giving iOS a reason to keep the process runnable while
    /// the encrypted mailbox is fetched and acknowledged.
    private var remoteRelayWakeCompletions: [UUID: (Bool) -> Void] = [:]
    /// C2: whether a full core-engine pass has run to completion in this
    /// process, feeding `CoreRelayPassPlan.sweptThisSession` — an input rather
    /// than store state because `relay_sweep_due` correctness must not depend on
    /// recovering an in-memory session. Touched only on the detached relay task,
    /// which serialises through `relaySyncInFlight`.
    private var coreEngineSweptThisSession = false
    /// C2: the read-only migration canary. On a sampled few legacy passes a day
    /// it captures what the legacy engine observed for the receipts+authored
    /// slice and asks the core planner what it would have done, recording only
    /// where they differ. It holds a diagnostics sink, never the store, and can
    /// open no socket. Off for a core pass (`relayShadowPermitted`).
    private lazy var relayShadowAdapter = RelayShadowAdapter(
        sink: RelayShadowReportSink { report, nowMs in
            AppStore.get().noteRelayShadowReport(report: report, nowMs: nowMs)
        },
        passEngine: { RelayEngineSettings.passEngine() },
        shadowEnabled: { RelayEngineSettings.shadowEnabled() },
        loadSampler: { RelayEngineSettings.shadowSampler() },
        saveSampler: { RelayEngineSettings.setShadowSampler($0) }
    )
    private var currentLanEndpoint: LanManualEndpoint?
    private var currentLanInstanceToken: Data?
    private var currentLanNetworkId: String?
    private var appForeground = true

    private init() {}

    // MARK: - Threading helpers

    /// Hands a closure to the main actor for UI-facing state.
    ///
    /// `DispatchQueue.main.async` rather than `Task { @MainActor in }`: the
    /// mesh queue emits these in a meaningful order (a disconnect's nearby
    /// refresh must not land before the connect's), and queue `async` is FIFO
    /// where task enqueueing carries no such promise.
    private func onMain(_ body: @escaping @MainActor @Sendable () -> Void) {
        DispatchQueue.main.async { MainActor.assumeIsolated(body) }
    }

    /// Runs `body` on `meshQueue` and returns its result, for the one caller
    /// that needs a value back: the relay sync pass's per-envelope
    /// `processInboundEnvelope`. Suspends rather than blocking, so a pass
    /// waiting behind a burst of BLE frames never occupies a pool thread.
    ///
    /// Each envelope is still handled start-to-finish inside one queue block,
    /// which is all DTN D4 requires; interleaving *between* envelopes with
    /// BLE/LAN frames is exactly what the previous `MainActor.run` per
    /// envelope did too.
    private func onMeshQueue<T>(_ body: @escaping () -> T) async -> T {
        await withCheckedContinuation { continuation in
            meshQueue.async { continuation.resume(returning: body()) }
        }
    }

    /// A timer that fires on `meshQueue`, replacing `Timer.scheduledTimer`
    /// (which needs a run loop, and so pinned all three of this class's timers
    /// to the main thread). `cancel()` is the `invalidate()` twin.
    private func meshTimer(
        intervalSeconds: TimeInterval,
        repeats: Bool,
        _ body: @escaping @Sendable () -> Void
    ) -> DispatchSourceTimer {
        let timer = DispatchSource.makeTimerSource(queue: meshQueue)
        if repeats {
            timer.schedule(deadline: .now() + intervalSeconds, repeating: intervalSeconds)
        } else {
            timer.schedule(deadline: .now() + intervalSeconds)
        }
        timer.setEventHandler(handler: body)
        timer.resume()
        return timer
    }

    // MARK: - Lifecycle

    func configure(identity: Identity) {
        meshQueue.async {
            self.identity = identity
            MeshRouter.setLocalUserId(identity.userId)
        }
    }

    func start() {
        meshQueue.async { self.startOnMeshQueue() }
    }

    func stop() {
        meshQueue.async { self.stopOnMeshQueue() }
    }

    func setAppForeground(_ foreground: Bool) {
        meshQueue.async { self.setAppForegroundOnMeshQueue(foreground) }
    }

    /// §9.4's radio silence, applied and then reported back.
    ///
    /// A device between "the channel is confirmed" and "the roster head is
    /// acknowledged" may not advertise anything, and core cannot enforce that for
    /// radios it has never heard of. `LinkVisibility` reads the gate; this is the
    /// hand that turns them off, and `completion` is how the ceremony learns the
    /// silence is real rather than merely requested.
    ///
    /// The whole controller goes down rather than only the two radios, because
    /// the BLE transport and the LAN transport are built together in
    /// `startOnMeshQueue` and there is no seam that stops one and keeps the rest.
    /// That is strictly more silence than §9.4 asks for, which is the safe
    /// direction — §9.4 forbids acking and authoring over the relay in the same
    /// breath. `linkSilenced` is what keeps the allow branch from *starting* a
    /// mesh the person had switched off themselves.
    func applyLinkVisibility(_ allowed: Bool, completion: @escaping () -> Void) {
        meshQueue.async {
            if allowed {
                if self.linkSilenced {
                    self.linkSilenced = false
                    self.startOnMeshQueue()
                }
            } else if self.isRunning {
                self.linkSilenced = true
                self.stopOnMeshQueue()
            }
            completion()
        }
    }

    /// The contact list changed (a contact was deleted, blocked, or
    /// unblocked), so anything derived from it needs rebuilding.
    func contactListChanged() {
        meshQueue.async { self.refreshLanCapableContacts() }
    }

    func notifyChatViewed(chatId: Data) {
        meshQueue.async { self.notifyChatViewedOnMeshQueue(chatId: chatId) }
    }

    func handleRemoteRelayWake(completion: @escaping (Bool) -> Void) {
        meshQueue.async {
            let id = UUID()
            self.remoteRelayWakeCompletions[id] = completion
            self.meshQueue.asyncAfter(deadline: .now() + 25) {
                self.finishRemoteRelayWake(id: id, completed: false)
            }
            self.attemptRemoteRelayWake(id: id)
        }
    }

    private func attemptRemoteRelayWake(id: UUID) {
        guard remoteRelayWakeCompletions[id] != nil else { return }
        guard !runRelaySync() else { return }
        // A background launch can reach this queue before NWPathMonitor has
        // delivered its first satisfied path. Keep the OS-granted window open
        // and retry briefly instead of reporting failure immediately and
        // getting suspended just before connectivity becomes visible.
        meshQueue.asyncAfter(deadline: .now() + .milliseconds(500)) {
            self.attemptRemoteRelayWake(id: id)
        }
    }

    private func finishRemoteRelayWake(id: UUID, completed: Bool) {
        guard let completion = remoteRelayWakeCompletions.removeValue(forKey: id) else { return }
        onMain { completion(completed) }
    }

    private func finishAllRemoteRelayWakes(completed: Bool) {
        let completions = Array(remoteRelayWakeCompletions.values)
        remoteRelayWakeCompletions.removeAll()
        for completion in completions {
            onMain { completion(completed) }
        }
    }

    private func startOnMeshQueue() {
        // specs/multi-device-v1.md §10 step 5: a device its person removed does
        // not come back up. Core's gate would refuse everything the mesh does
        // anyway, but a running controller reporting "Meshing" is its own claim,
        // and both the app and a background BLE relaunch reach here. One select
        // against a single-row table, on the queue that owns this state.
        DeviceRemovalStatus.shared.refresh(store: store)
        if DeviceRemovalStatus.shared.isRemoved {
            log.warning("This device was removed from its person's devices; not starting the mesh")
            // The preference too, and here rather than only in the root view:
            // a background BLE relaunch may never show any UI at all, and
            // `startMeshIfEnabled` would keep trying on every one of them.
            stopBecauseThisDeviceWasRemoved()
            return
        }
        if isRunning {
            // Repeat start while already running: refresh status only.
            let nearby = MeshRouter.connectedUserCount()
            onMain { MeshRuntimeStatus.shared.markMeshing(nearby: nearby) }
            return
        }
        isRunning = true
        onMain { MeshRuntimeStatus.shared.markStarting() }
        // Spray decisions carry no store of their own, so this is where they
        // find the protocol-event ring. Before the transports come up, so the
        // first reconnect of the session is already recorded. Android does the
        // same, in `MeshService`.
        SprayPolicy.attachEventJournal(store: store)

        MeshRouter.registerCentral { [weak self] address, frame in
            self?.transport.sendAsCentral(address: address, frame: frame)
        }
        MeshRouter.registerPeripheral { [weak self] address, frame in
            self?.transport.sendAsPeripheral(address: address, frame: frame)
        }
        let lan = LanTransport(
            identity: identity,
            trustedPeerForStaticKey: { [weak self, store = self.store] remoteStaticKey in
                if let userId = trustedLanPeerUserId(
                    contacts: (try? store.listContacts()) ?? [],
                    remoteStaticKey: remoteStaticKey
                ) {
                    return userId
                }
                self?.recordOwnIdentityCloneIfAuthenticated(remoteStaticKey: remoteStaticKey)
                return nil
            },
            ownDeviceLanProof: { [weak self] handshakeHash, role in
                self?.ownDeviceLanProof(handshakeHash: handshakeHash, role: role)
            },
            openOwnDeviceLanProof: { [weak self] handshakeHash, payload, peerRole in
                self?.openOwnDeviceLanProof(
                    handshakeHash: handshakeHash,
                    payload: payload,
                    peerRole: peerRole
                )
            }
        )
        lanTransport = lan
        LanTransportDiagnostics.shared.register { [weak lan] endpoint in
            lan?.connect(endpoint, manual: true)
        }
        MeshRouter.registerLan { [weak lan] address, frame in
            lan?.sendFrame(address: address, frame: frame)
        }
        lan.onNetworkReady = { [weak self, weak lan] endpoint, instanceToken, networkId in
            self?.meshQueue.async {
                guard let self, let lan, self.isRunning else { return }
                self.currentLanEndpoint = endpoint
                self.currentLanInstanceToken = instanceToken
                self.currentLanNetworkId = networkId
                // A new Wi-Fi is a fresh reason to look for a device of this
                // person's own: every peer on it has to be found again from
                // nothing, and the shortfall this phone was carrying is not
                // itself news.
                self.ownDeviceSearchWindow.rearm(nowMs: Int64(Date().timeIntervalSince1970 * 1_000))
                lan.updateOwnDeviceSearchLive(true)
                for contact in (try? self.store.listContacts()) ?? [] {
                    // This phone's own address on the network it just joined
                    // is what lets the cache throw out an unproven entry that
                    // belongs to some other subnet -- the entries shipped
                    // builds filed, each of which costs a connect timeout on
                    // every join until it does.
                    if let cached = LanEndpointCache.load(
                        networkId: networkId,
                        userId: contact.userId,
                        localHost: endpoint.host
                    ) {
                        lan.connect(cached, cached: true)
                    }
                }
                LanEndpointSender.queueToAllCapableContacts(
                    store: self.store,
                    identity: self.identity,
                    endpoint: endpoint,
                    instanceToken: instanceToken,
                    networkId: networkId
                )
                for route in MeshRouter.selectedIdentifiedRoutes() {
                    self.sendLanEndpointHint(address: route.address)
                }
            }
        }
        lan.onAuthenticated = { [weak self] address, userId, dialedEndpoint in
            self?.meshQueue.async {
                guard let self, self.isRunning else { return }
                let previouslySelectedAddress = MeshRouter.routeFor(userId: userId)?.1
                MeshRouter.onConnected(address: address, transport: .lan)
                guard MeshRouter.onHello(address: address, userId: userId) else { return }
                let seenAtMs = Int64(Date().timeIntervalSince1970 * 1_000)
                self.onMain {
                    MeshConnectivityStatus.shared.mergeLastSeen(userId: userId, seenAtMs: seenAtMs)
                }
                let name = (try? self.store.getContact(userId: userId))?.name
                    ?? String(UserIdHex.encode(userId).prefix(8))
                if (try? self.store.getContact(userId: userId)) != nil {
                    // An authenticated LAN link is the strongest possible
                    // evidence this contact shares a LAN with us, so it also
                    // refreshes the capability recency the automatic-scan
                    // gate reads.
                    LanCapabilityStore.markSupported(userId: userId)
                    self.refreshLanCapableContacts()
                    self.recordPeerConnection(
                        userId: userId,
                        transport: .lan,
                        kind: .connected
                    )
                }
                // A completed Noise handshake is the proof a hint never had,
                // so the address is filed as authenticated -- promoting
                // whatever unproven entry was already there. That, and only
                // that, lets it survive a later load on a routed LAN where
                // this phone cannot see itself on the peer's subnet. It
                // mirrors `isSingleShotLanConnectKey`'s rule above: an
                // address that answered is evidence, a claim about one is not.
                if let dialedEndpoint {
                    LanEndpointCache.save(
                        networkId: self.currentLanNetworkId,
                        userId: userId,
                        endpoint: dialedEndpoint,
                        provenance: .authenticated
                    )
                }
                LanTransportDiagnostics.shared.authenticated(address: address, peerName: name)
                self.sendHello(address: address)
                if MeshRouter.routeFor(userId: userId)?.1 != previouslySelectedAddress {
                    // Authentication already proves the LAN peer's identity.
                    // Continue immediately on the promoted route rather than
                    // waiting for its wire HELLO or periodic maintenance.
                    self.resumeLogicalPeerSync(peerUserId: userId)
                }
                self.sendLanEndpointHint(address: address)
                self.queueCurrentLanEndpoint(to: userId)
                self.refreshNearby()
            }
        }
        lan.onOwnDeviceAuthenticated = { [weak self] address in
            self?.meshQueue.async {
                guard let self, self.isRunning else { return }
                self.onOwnDeviceLanLink(address: address)
            }
        }
        lan.onDisconnected = { [weak self] address in
            self?.meshQueue.async {
                guard let self, self.isRunning else { return }
                let peerUserId = MeshRouter.userIdFor(address: address)
                let wasSelected = MeshRouter.isSelectedRoute(address: address)
                self.recordPeerDisconnected(address: address)
                self.lanHealth.remove(address: address)
                self.ownRosterNoticeSchedule.forget(address: address)
                LanTransportDiagnostics.shared.disconnected(address: address)
                MeshRouter.onDisconnected(address: address)
                if wasSelected, let peerUserId {
                    self.scheduleFailoverResume(peerUserId: peerUserId)
                }
                self.refreshNearby()
            }
        }
        lan.onFrame = { [weak self] address, frame in
            self?.meshQueue.async {
                guard let self, self.isRunning else { return }
                self.onFrameReceived(address: address, frame: frame)
            }
        }
        lan.start(foregroundActive: appForeground)
        refreshLanCapableContacts()
        startLanHealthLoop()
        startDigestMaintenanceLoop()

        transport.onFrame = { [weak self] address, frame in
            self?.meshQueue.async { self?.onFrameReceived(address: address, frame: frame) }
        }
        transport.onCentralConnected = { [weak self] address in
            self?.meshQueue.async {
                MeshRouter.onConnected(address: address, transport: .central)
                self?.sendHello(address: address)
                self?.refreshNearby()
            }
        }
        transport.onCentralDisconnected = { [weak self] address in
            // FI6: hopped onto `meshQueue` exactly like the connect callbacks
            // above, and for the same reason. Both callbacks arrive on
            // `BleTransport`'s own serial queue, and `DispatchQueue.async`
            // enqueues FIFO, so a fast connect->disconnect cannot have its
            // disconnect processed first and re-register a dead route. The
            // queue makes this stronger than the `Task { @MainActor }` hop it
            // replaces: frames land in the same queue as connection events, so
            // the whole mesh event stream keeps one order.
            self?.meshQueue.async {
                let peerUserId = MeshRouter.userIdFor(address: address)
                let wasSelected = MeshRouter.isSelectedRoute(address: address)
                MeshController.shared.recordPeerDisconnected(address: address)
                MeshRouter.onDisconnected(address: address)
                if wasSelected, let peerUserId {
                    MeshController.shared.scheduleFailoverResume(peerUserId: peerUserId)
                }
                MeshController.shared.refreshNearby()
            }
        }
        transport.onPeripheralSubscribed = { [weak self] address in
            self?.meshQueue.async {
                MeshRouter.onConnected(address: address, transport: .peripheral)
                self?.sendHello(address: address)
                self?.refreshNearby()
            }
        }
        transport.onPeripheralUnsubscribed = { [weak self] address in
            self?.meshQueue.async {
                let peerUserId = MeshRouter.userIdFor(address: address)
                let wasSelected = MeshRouter.isSelectedRoute(address: address)
                MeshController.shared.recordPeerDisconnected(address: address)
                MeshRouter.onDisconnected(address: address)
                if wasSelected, let peerUserId {
                    MeshController.shared.scheduleFailoverResume(peerUserId: peerUserId)
                }
                MeshController.shared.refreshNearby()
            }
        }

        registerBluetoothAudioObserver()
        startRelayLoop()
        startMeshRoles()
        // §9.4, for the phone that was killed mid-ceremony: core still refuses
        // everything it holds, and this is what stops the radios disagreeing
        // with it. Asked here rather than only from `LinkSession` because a
        // process that died inside the pre-activation window comes back with the
        // window still open and no ceremony left to ask on its behalf. Costs one
        // gate read per mesh start and changes nothing on the overwhelming
        // majority of installs, which have never linked a device. Mirrors
        // Android's `MeshService`, which refreshes at start and on its tick.
        LinkVisibility.refresh(store: store)
        refreshBluetoothAudioState(reason: "mesh start")
        let nearby = MeshRouter.connectedUserCount()
        onMain { MeshRuntimeStatus.shared.markMeshing(nearby: nearby) }
        log.info("Mesh started")
    }

    private func stopOnMeshQueue() {
        guard isRunning else { return }
        isRunning = false
        bluetoothAudioConnected = false
        bluetoothAudioBackoff.reset()
        unregisterBluetoothAudioObserver()
        lanTransport?.stop()
        lanTransport = nil
        LanTransportDiagnostics.shared.unregister()
        lanHealthTimer?.cancel()
        lanHealthTimer = nil
        lanHealth.clear()
        ownRosterNoticeSchedule.clear()
        ownDeviceSearchWindow.clear()
        // A debounced failover resume can still be queued on meshQueue. Its
        // block re-checks `isRunning` (already false above) before doing any
        // work, so it cannot outlive this; clearing the armed windows just
        // keeps the state honest for the next start.
        failoverResumeDebounce.clear()
        digestMaintenanceTimer?.cancel()
        digestMaintenanceTimer = nil
        for pending in pendingSprayDeferrals.values { pending.cancel() }
        pendingSprayDeferrals.removeAll()
        gatedDigests.removeAll()
        SprayPolicy.reset()
        currentLanEndpoint = nil
        currentLanInstanceToken = nil
        currentLanNetworkId = nil
        stopMeshRoles()
        MeshRouter.unregisterCentral()
        MeshRouter.unregisterPeripheral()
        MeshRouter.unregisterLan()
        MeshRouter.reset()
        onMain { MeshConnectivityStatus.shared.clear() }
        relayTimer?.cancel()
        relayTimer = nil
        relayRateLimitedUntilMs = 0
        relayRateLimitRetryWorkItem?.cancel()
        relayRateLimitRetryWorkItem = nil
        relayMailboxContinuationWorkItem?.cancel()
        relayMailboxContinuationWorkItem = nil
        familyRelayBackoff.onSuccessfulPass()
        lastKnownPushHealthy = nil
        pathMonitor?.cancel()
        pathMonitor = nil
        relayPushClient.stop()
        relayCancellable?.cancel()
        relayCancellable = nil
        relaySyncPending = false
        finishAllRemoteRelayWakes(completed: false)
        onMain { MeshRuntimeStatus.shared.markStopped() }
        log.info("Mesh stopped")
    }

    private func setAppForegroundOnMeshQueue(_ foreground: Bool) {
        let changed = appForeground != foreground
        appForeground = foreground
        lanTransport?.setForegroundActive(foreground)
        // Battery, 2026-07-21: lanHealthTimer and digestMaintenanceTimer are
        // foreground-only (see their docs) -- background execution windows
        // are kept alive by CoreBluetooth activity alone, and neither tick
        // does anything BLE frame relay needs while backgrounded. Re-running
        // both start*Loop functions on any foreground/background flip stops
        // them immediately on backgrounding and, on returning to foreground,
        // fires an immediate catch-up tick (via their own guard) before
        // resuming the normal interval -- see startLanHealthLoop /
        // startDigestMaintenanceLoop. relayTimer is different: it must keep
        // running in the background (see RelayPushClient's class doc), but
        // RelayPollPolicy's backoff only ever applies while foregrounded
        // (see its doc), so a flip reschedules it immediately rather than
        // waiting out whatever long interval is already pending --
        // backgrounding hands the poll sole responsibility for relay
        // delivery.
        if changed, isRunning {
            startLanHealthLoop()
            startDigestMaintenanceLoop()
            reschedulePoll(currentlyHealthy: relayPushClient.isHealthy())
        }
    }

    // MARK: - Bluetooth audio coexistence

    private func registerBluetoothAudioObserver() {
        guard audioRouteObserver == nil else { return }
        audioRouteObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.routeChangeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.meshQueue.async {
                self?.refreshBluetoothAudioState(reason: "route change")
            }
        }
    }

    private func unregisterBluetoothAudioObserver() {
        if let audioRouteObserver {
            NotificationCenter.default.removeObserver(audioRouteObserver)
            self.audioRouteObserver = nil
        }
    }

    /**
     Records whether Bluetooth audio is routed. It no longer changes the mesh:
     Android dropped that policy on 2026-07-09 because messaging was dead on a
     phone whenever earbuds were connected, and iOS has strictly less control
     over its radio than Android does, so there is no iOS-specific knob whose
     absence would justify a stricter rule here.
     */
    private func refreshBluetoothAudioState(reason: String) {
        guard isRunning else { return }
        switch bluetoothAudioBackoff.update(bluetoothAudioActive: isBluetoothAudioActive()) {
        case .audioClear:
            bluetoothAudioConnected = false
            log.info("Bluetooth audio route cleared (\(reason, privacy: .public))")
        case .audioConnected:
            bluetoothAudioConnected = true
            log.info("Bluetooth audio routed; mesh stays up (\(reason, privacy: .public))")
        case nil:
            return
        }
        let connected = bluetoothAudioConnected
        onMain { MeshRuntimeStatus.shared.setBluetoothAudioConnected(connected) }
    }

    /// Active Bluetooth audio route (A2DP / HFP / LE audio). See `BluetoothAudioBackoff`.
    private func isBluetoothAudioActive() -> Bool {
        let outputs = AVAudioSession.sharedInstance().currentRoute.outputs
        return outputs.contains { port in
            switch port.portType {
            case .bluetoothA2DP, .bluetoothHFP, .bluetoothLE:
                return true
            default:
                return false
            }
        }
    }

    private func startMeshRoles() {
        guard !meshRolesRunning else { return }
        transport.start()
        meshRolesRunning = true
        // DL-3's third trigger, and the only one that works with no internet at
        // all. The relay pass fires the same idempotent call, but a person who
        // adds a friend on a ship with no Wi-Fi and no pass never reaches a relay
        // pass — and the friend they just made would never learn which devices
        // they have. The envelopes this queues ride BLE, LAN and carry like any
        // other sealed mail. On the install that has never linked a device, which
        // is nearly the whole fleet, it reads one row and returns. Mirrors
        // Android's `MeshService.startMeshRoles`.
        if let identity {
            RosterGossipSender.announceIfOwed(store: store, identity: identity)
        }
    }

    private func stopMeshRoles() {
        guard meshRolesRunning else { return }
        transport.stop()
        meshRolesRunning = false
        MeshRouter.resetBle()
    }

    private func notifyChatViewedOnMeshQueue(chatId: Data) {
        guard let identity else { return }
        guard let contact = try? store.getContact(userId: chatId) else {
            notifyGroupViewed(groupId: chatId)
            return
        }
        let through = PeerStreamWatermark.through(store: store, chatId: chatId, senderUserId: chatId)
        guard through > 0 else { return }
        try? store.recordOutgoingReceipt(
            chatId: chatId,
            senderUserId: chatId,
            receiptType: ReceiptType.read,
            throughLamport: through
        )
        _ = queueOutgoingReceiptForRelay(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.read,
            ackedSenderUserId: chatId,
            throughLamport: through
        )
        sendReceiptToContact(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.read,
            ackedSenderUserId: chatId,
            throughLamport: through
        )
        RelaySyncEvents.requestSync()
    }

    private func notifyGroupViewed(groupId: Data) {
        guard let identity,
              let group = try? store.getGroup(groupId: groupId),
              group.memberUserIds.contains(identity.userId) else { return }
        for senderUserId in group.memberUserIds where senderUserId != identity.userId {
            let through = PeerStreamWatermark.through(
                store: store,
                chatId: groupId,
                senderUserId: senderUserId
            )
            guard through > 0 else { continue }
            try? store.recordOutgoingReceipt(
                chatId: groupId,
                senderUserId: senderUserId,
                receiptType: ReceiptType.read,
                throughLamport: through
            )
            emitGroupReceiptsToAuthor(group: group, authorUserId: senderUserId, identity: identity)
        }
    }

    // MARK: - Frames

    private func sendHello(address: String) {
        guard let identity else { return }
        MeshRouter.sendToAddress(address: address, frame: encodeHello(userId: identity.userId))
        // HELLO2 rides right behind the legacy HELLO: capability bits for
        // the hidden-kind spray bound. Pre-HELLO2 builds reject the unknown
        // frame type and drop it without touching the link.
        if let hello2 = try? encodeHello2(userId: identity.userId, capabilities: coreOwnCapabilities()) {
            MeshRouter.sendToAddress(address: address, frame: hello2)
        }
    }

    private func onFrameReceived(address: String, frame: Data) {
        guard let identity else { return }
        let parsed: Frame
        do {
            parsed = try parseFrame(bytes: frame)
        } catch {
            log.warning("Unparseable frame from \(address, privacy: .public)")
            return
        }
        switch parsed {
        case .hello(let userId):
            handleHello(address: address, userId: userId, identity: identity)
        case .hello2(let userId, let capabilities):
            if noteOwnIdentityHello(address: address, userId: userId, identity: identity) {
                // A device of this person's own, on a link that has proved it:
                // §10 step 5's meeting. Here rather than in the legacy HELLO
                // case because the capability bits only ride 0x06. Remembered
                // as well as acted on: the meeting is the only moment those
                // bits cross the wire, and `probeLanLinks` re-offers on this
                // link long after it.
                ownRosterNoticeSchedule.noteHello2(
                    address: address,
                    capabilities: capabilities
                )
                offerOwnRosterNotice(
                    address: address,
                    capabilities: capabilities,
                    identity: identity
                )
            } else {
                MeshRouter.onHello2(address: address, userId: userId, capabilities: capabilities)
            }
        case .ownRoster(let document):
            handleOwnRosterNotice(address: address, document: document, identity: identity)
        case .envelope(let msgId, let hopTtl, let expiry, let recipientHint, let sealed):
            processInboundEnvelope(
                sourceAddress: address,
                msgId: msgId,
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed,
                identity: identity
            )
        case .digest(let chatId, let entries, let recentMsgIds):
            handleDigest(
                address: address,
                chatId: chatId,
                entries: entries,
                recentMsgIds: recentMsgIds,
                identity: identity
            )
        case .lanEndpoint(let instanceToken, let host, let port):
            handleLanEndpointHint(
                address: address,
                instanceToken: instanceToken,
                endpoint: LanManualEndpoint(host: host, port: port)
            )
        case .transportProbe(let nonce, let response):
            handleTransportProbe(address: address, nonce: nonce, response: response)
        }
    }

    private func sendLanEndpointHint(address: String) {
        guard let endpoint = currentLanEndpoint,
              let instanceToken = currentLanInstanceToken,
              let frame = try? encodeLanEndpoint(
                instanceToken: instanceToken,
                host: endpoint.host,
                port: endpoint.port
              ) else { return }
        _ = MeshRouter.sendToAddress(address: address, frame: frame)
    }

    private func queueCurrentLanEndpoint(to userId: Data) {
        guard let identity,
              let endpoint = currentLanEndpoint,
              let instanceToken = currentLanInstanceToken,
              let networkId = currentLanNetworkId,
              LanCapabilityStore.isSupported(userId: userId),
              let contact = try? store.getContact(userId: userId) else { return }
        LanEndpointSender.queueToContact(
            store: store,
            identity: identity,
            contact: contact,
            endpoint: endpoint,
            instanceToken: instanceToken,
            networkId: networkId
        )
    }

    private func handleLanEndpointHint(
        address: String,
        instanceToken: Data,
        endpoint: LanManualEndpoint
    ) {
        guard let userId = MeshRouter.userIdFor(address: address),
              (try? store.getContact(userId: userId)) != nil else { return }
        LanCapabilityStore.markSupported(userId: userId)
        refreshLanCapableContacts()
        // The dial below is deliberately attempted even when the hinted host
        // is on another subnet -- a routed LAN can carry TCP where Bonjour
        // cannot, and it is one bounded attempt Noise authenticates. Filing
        // that address is a different matter: the cache is keyed by THIS
        // phone's network, is re-dialed on every Wi-Fi join and lives for
        // seven days, so a foreign-subnet host written here becomes a
        // standing probe of an address that can never answer on this network.
        if lanHintMayBeCached(
            localHost: currentLanEndpoint?.host,
            candidateHost: endpoint.host
        ) {
            // Still only a claim until a handshake completes, so it is filed
            // unproven and re-checked against the network on every load.
            LanEndpointCache.save(
                networkId: currentLanNetworkId,
                userId: userId,
                endpoint: endpoint,
                provenance: .hinted
            )
        }
        queueCurrentLanEndpoint(to: userId)
        guard MeshRouter.transportFor(address: address) != .lan else { return }
        lanTransport?.connect(endpoint, remoteInstanceToken: instanceToken)
    }

    /// Pushes the contacts that have demonstrated LAN support into the
    /// transport, each with the millisecond that support was last seen. The
    /// automatic-scan gate keeps sweeping while any of them is not linked
    /// over LAN and its evidence is still recent.
    ///
    /// Blocked contacts are excluded, and a contact that no longer exists
    /// simply isn't in the list, so deleting or blocking someone lets the
    /// gate close. Also runs on the periodic LAN health tick, which is what
    /// makes that true without every screen having to say so.
    ///
    /// The reading itself is a contact-list query plus a stored value per
    /// contact, and it stays on its own utility task rather than occupying
    /// `meshQueue` for the length of a whole-contact-list sweep -- inbound
    /// frames queued behind it would wait for no reason, since nothing about
    /// this reading is ordered against them. The result is applied back on
    /// `meshQueue` because `lanTransport` belongs to it; the transport's own
    /// setter is queue-hopping and safe to call from there.
    private func refreshLanCapableContacts() {
        guard lanTransport != nil else { return }
        Task.detached(priority: .utility) { [weak self] in
            let capable = MeshController.lanCapableContacts()
            let roster = MeshController.ownRoster()
            guard let self else { return }
            self.meshQueue.async {
                self.lanTransport?.updateLanCapableContacts(capable)
                // The LAN transport's other sweep motive. A sibling shares this
                // person's user id, so it has no contact row and can never
                // appear in `capable` however long it waits -- and the motive is
                // a bounded window rather than the bare shortfall, because a
                // sibling that is switched off is missing forever. The roster is
                // fingerprinted rather than counted because a *removal* lowers
                // the shortfall: the phone holding the signed news for a removed
                // device would otherwise have no motive to go looking for it.
                let linked = MeshRouter.ownDeviceLinks().count
                self.ownDeviceSearchWindow.observe(
                    rosterFingerprint: roster.fingerprint,
                    unlinkedOwnDevices: max(0, roster.siblings - linked),
                    nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
                )
                self.lanTransport?.updateOwnDeviceSearchLive(self.ownDeviceSearchWindow.isLive)
            }
        }
    }

    /// This person's device roster: how many devices it lists besides this one,
    /// and an identity for the roster itself. See `OwnDeviceSearchWindow`.
    private static func ownRoster() -> (siblings: Int, fingerprint: String) {
        guard let fleet = try? AppStore.get().ownDeviceFleet() else {
            return (0, ownRosterFingerprint(deviceIds: []))
        }
        return (
            max(0, fleet.deviceIds.count - (fleet.ownDeviceId == nil ? 0 : 1)),
            ownRosterFingerprint(deviceIds: fleet.deviceIds)
        )
    }

    private static func lanCapableContacts() -> [Data: Int64] {
        let store = AppStore.get()
        let contacts = (try? store.listContacts()) ?? []
        let blocked = Set((try? store.listBlockedUsers()) ?? [])
        var capable: [Data: Int64] = [:]
        for contact in contacts where !blocked.contains(contact.userId) {
            if let lastSupportedAtMs = LanCapabilityStore.lastSupportedAtMs(userId: contact.userId) {
                capable[contact.userId] = lastSupportedAtMs
            }
        }
        return capable
    }

    private func handleTransportProbe(address: String, nonce: UInt64, response: Bool) {
        guard MeshRouter.transportFor(address: address) == .lan else { return }
        if response {
            let now = Int64(Date().timeIntervalSince1970 * 1_000)
            if let latency = lanHealth.response(address: address, nonce: nonce, nowMs: now) {
                LanTransportDiagnostics.shared.probeSucceeded(latencyMs: latency)
            }
        } else {
            _ = MeshRouter.sendToAddress(
                address: address,
                frame: encodeTransportProbe(nonce: nonce, response: true)
            )
        }
    }

    /// Battery, 2026-07-21: foreground-only (see `setAppForeground`). This
    /// only probes LAN links -- `probeLanLinks` filters
    /// `MeshRouter.identifiedRoutes()` down to `.lan` transport, and
    /// `LanTransport` itself already suspends its automatic-scan/discovery
    /// activity while backgrounded (`setForegroundActive`), so there is
    /// nothing background-live for this to usefully probe. Invalidates and
    /// returns (no timer) when backgrounded; fires an immediate catch-up
    /// probe before arming the repeating timer whenever this runs while
    /// foregrounded, so a foreground return doesn't wait out a stale 30s.
    ///
    /// **§10 step 5 rides this loop, and inherits the foreground gate — a
    /// platform difference, not an oversight.** Android's equivalent runs
    /// under a foreground service, so its own-device heartbeat and its roster
    /// re-offer keep going with the app off screen; here they converge only
    /// while CruiseMesh is on screen. That is what iOS allows: the LAN
    /// transport suspends discovery when backgrounded anyway
    /// (`LanTransport.setForegroundActive`), so a background re-offer would
    /// have no link to write to and a background probe nothing live to probe.
    /// `specs/multi-device-v1.md` §10 step 5 states it too; anyone reading the
    /// convergence claim should read it as "within minutes of either phone
    /// being opened" on this platform.
    private func startLanHealthLoop() {
        lanHealthTimer?.cancel()
        lanHealthTimer = nil
        guard appForeground else { return }
        probeLanLinks()
        lanHealthTimer = meshTimer(intervalSeconds: 30, repeats: true) { [weak self] in
            self?.refreshLanCapableContacts()
            self?.probeLanLinks()
        }
    }

    private func probeLanLinks() {
        let ownDeviceLinks = MeshRouter.ownDeviceLinks()
        // A link to one of this person's own devices is never a route, so it
        // was in none of the accessors this loop used to read -- see
        // `lanHealthProbeAddresses`, which is where that selection now lives so
        // a test can hold it in place.
        let addresses = lanHealthProbeAddresses(
            identifiedRoutes: MeshRouter.identifiedRoutes(),
            ownDeviceLinks: ownDeviceLinks
        )
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        for address in addresses {
            switch lanHealth.next(
                address: address,
                nowMs: now,
                nonce: UInt64.random(in: 1...UInt64.max)
            ) {
            case .send(let nonce):
                _ = MeshRouter.sendToAddress(
                    address: address,
                    frame: encodeTransportProbe(nonce: nonce, response: false)
                )
            case .wait:
                break
            case .close:
                lanTransport?.closeConnection(address: address)
                LanTransportDiagnostics.shared.probeFailed(
                    "The encrypted LAN link stopped responding and was reconnected"
                )
            }
        }
        reofferOwnRosterNotices(ownDeviceLinks: ownDeviceLinks)
    }

    /// **§10 step 5, level-triggered.** Re-offer this person's roster on every
    /// live own-device link that is due one, and nudge any link whose peer
    /// HELLO2 never arrived.
    ///
    /// The notice shipped edge-triggered on an inbound HELLO2 and nothing else,
    /// so a removal that happened while a sibling link was *already up* was
    /// never announced on it. Nothing else in either shell pushed one: no
    /// roster-change hook, no periodic re-offer, and HELLO is sent only when a
    /// link is established. See `OwnRosterNoticeSchedule` for why this is a
    /// timer rather than an event, and `ownRosterNoticeTargets` /
    /// `ownDeviceLinksAwaitingHello2` for the selection this delegates.
    ///
    /// Mirrors Android's `MeshService.reofferOwnRosterNotices`.
    private func reofferOwnRosterNotices(
        ownDeviceLinks: [(transport: MeshRouterState.Transport, address: String)]
    ) {
        guard let identity else { return }
        let nowMs = Int64(Date().timeIntervalSince1970 * 1_000)
        for target in ownRosterNoticeTargets(
            ownDeviceLinks: ownDeviceLinks,
            schedule: ownRosterNoticeSchedule,
            nowMs: nowMs
        ) {
            offerOwnRosterNotice(
                address: target.address,
                capabilities: target.capabilities,
                identity: identity
            )
        }
        for address in ownDeviceLinksAwaitingHello2(
            ownDeviceLinks: ownDeviceLinks,
            schedule: ownRosterNoticeSchedule
        ) {
            sendHello(address: address)
        }
    }

    /// A device of this person's own, on this Wi-Fi, on a link that proved it
    /// (`specs/multi-device-v1.md` §10 step 5).
    ///
    /// Registered as a transport so frames can be written to it, and deliberately
    /// **not** as a route: the user id on the other end is this person's own, and
    /// a route to this person would hand this phone's own mail straight back out.
    ///
    /// Registered through `MeshRouter.onOwnDeviceConnected` rather than
    /// `MeshRouter.onConnected`, and the difference is load-bearing. Simply
    /// withholding a user id is not enough: core's epidemic fanout floods every
    /// *not yet* identified link on purpose, so an unmarked link would receive
    /// every envelope this phone sends or relays — a live feed for a device the
    /// person may have just removed. Marked, it is out of the fanout, and a
    /// HELLO naming a contact cannot win it that contact's route either. What is
    /// left is exactly what §10 step 5 needs: frames addressed to this one link.
    ///
    /// The HELLO pair goes out for the same reason it does on a peer link: the
    /// capability bits ride HELLO2, and the roster notice is answered from
    /// `onFrameReceived` once the other end has said what it understands.
    ///
    /// The clone warning is not raised here. It was already raised during the
    /// handshake (`recordOwnIdentityCloneIfAuthenticated`, through the
    /// trusted-peer lookup that returned nothing), which is the same evidence one
    /// moment earlier — raising it again would be one meeting counted twice.
    ///
    /// Mirrors Android's `MeshService.onOwnDeviceLanLink`.
    private func onOwnDeviceLanLink(address: String) {
        MeshRouter.onOwnDeviceConnected(address: address, transport: .lan)
        log.info("Another device of ours authenticated over local Wi-Fi")
        sendHello(address: address)
    }

    /// Whether this HELLO came from this person rather than from a peer — in
    /// which case it must not become a route, because a route to this person
    /// leads back to this phone.
    ///
    /// Two ways that can be true, and **the second one is why the 2026-08-24
    /// field failure survived every other repair.**
    ///
    /// - *The link proved it* (`ownIdentityLinkIsProven`). This is the sibling
    ///   case, and it is the only one that ever happens between two devices §9
    ///   linked. The old test could not see it: it asked whether the HELLO
    ///   *claimed our user id*, and a linked device has a user id of its own —
    ///   derived from its own signing key, because the ceremony never hands over
    ///   the person's. So a sibling's HELLO2 read as a stranger's, the roster
    ///   notice schedule never learned its capability bits, and
    ///   `dueCapabilities` returned nil forever.
    /// - *The frame claims our user id*, proven or not. That is the
    ///   `.cmbak`-clone meeting, or somebody asserting our identity on a
    ///   cleartext BLE HELLO. Both must be kept off the router; only the first is
    ///   believed anywhere else, and `ownIdentityLinkIsProven` separates them.
    ///
    /// The clone warning is not recorded here: the LAN handshake raised it one
    /// moment earlier for exactly this link
    /// (`recordOwnIdentityCloneIfAuthenticated`), and raising it again would
    /// count one meeting twice.
    ///
    /// Mirrors Android's `MeshService.noteOwnIdentityHello`.
    @discardableResult
    private func noteOwnIdentityHello(address: String, userId: Data, identity: Identity) -> Bool {
        if ownIdentityLinkIsProven(address: address, identity: identity) { return true }
        guard userId == identity.userId else { return false }
        log.warning("Ignoring unauthenticated HELLO that claims our identity")
        return true
    }

    /// This device's §10 step 5 proof for one finished LAN Noise session: a
    /// signature over that session's transcript hash under this device's roster
    /// signing key.
    ///
    /// Nil on an install that has never linked — no device key means no
    /// certificate in anybody's roster, so there is no sibling that could
    /// recognise this phone and nothing a proof could assert.
    ///
    /// **Nil also on a phone whose roster names no device but itself**, which is
    /// most phones. A proof is this device's stable signing public key with a
    /// signature attached, and the initiator puts it in front of a host it dialed
    /// before that host has proved anything — so a phone sweeping a ship's `/24`
    /// would hand a durable identifier to whatever answered on the port. A solo
    /// phone gains nothing for it: with nobody in its roster to recognise, the
    /// only proof it could ever open is its own coming back, and
    /// `coreOwnDeviceLanProofOpen` refuses that. So the key stays off the wire
    /// until this person actually has a fleet.
    ///
    /// Mirrors Android's `MeshService.ownDeviceLanProof`.
    private func ownDeviceLanProof(handshakeHash: Data, role: CoreLanProofRole) -> Data? {
        guard let device = DeviceKeyStore.load(),
              let roster = try? store.ownRoster(),
              coreRosterNamesASibling(roster: roster, ownDeviceId: device.deviceId) else {
            return nil
        }
        do {
            return try coreOwnDeviceLanProof(
                deviceSignSk: device.signSk,
                handshakeHash: handshakeHash,
                role: role
            )
        } catch {
            log.warning("Could not sign this phone's own-device proof")
            return nil
        }
    }

    /// Which of this person's devices the far end of a LAN session just proved it
    /// is, checked against the roster this phone holds.
    ///
    /// Nil for everything else, which is the overwhelming majority of what dials
    /// a phone on a shared Wi-Fi. The roster is the only place the answer comes
    /// from and it names nobody but this person's own devices, live and buried —
    /// a tombstoned one answers here on purpose, because it is the device the
    /// notice exists for.
    ///
    /// `peerRole` is the end the peer speaks from, and this device's own id goes
    /// in beside it. Together they are what stops a host this phone dialed from
    /// decrypting the proof it was just sent, re-encrypting it under its own
    /// sending key, and handing it back: both ends of one Noise session share a
    /// transcript hash, so without them that reflection verifies, names this
    /// phone, and is found in this phone's own roster.
    ///
    /// Mirrors Android's `MeshService.openOwnDeviceLanProof`.
    private func openOwnDeviceLanProof(
        handshakeHash: Data,
        payload: Data,
        peerRole: CoreLanProofRole
    ) -> CoreLanOwnDeviceProof? {
        guard let roster = try? store.ownRoster() else { return nil }
        return coreOwnDeviceLanProofOpen(
            roster: roster,
            handshakeHash: handshakeHash,
            payload: payload,
            peerRole: peerRole,
            ownDeviceId: DeviceKeyStore.load()?.deviceId ?? Data()
        )
    }

    /// Whether this link has proved, cryptographically, that the other end is
    /// this person's — either one of their devices or a clone of their identity.
    ///
    /// **This used to ask only the clone question**, and that is the second half
    /// of the 2026-08-24 field failure. Even with the transport's admission gate
    /// repaired, a sibling link would have been admitted and then found
    /// ineligible here, because a §9-linked device holds an agreement key of its
    /// own and can never present ours. The notice would have crossed no link at
    /// all, for exactly the reason it crossed none before.
    ///
    /// So the bar is what the transport actually established: a LAN link it
    /// admitted as one of this person's own, by roster proof or by the clone
    /// test, and nothing else. A cleartext BLE HELLO still never clears it.
    private func ownIdentityLinkIsProven(address: String, identity: Identity) -> Bool {
        let isLanLink = MeshRouter.transportFor(address: address) == .lan
        let transport = isLanLink ? lanTransport : nil
        return OwnRosterNoticePolicy.mayCross(
            isLanLink: isLanLink,
            ownAgreePk: identity.agreePk,
            sessionRemoteStaticKey: transport?.remoteStaticKey(address: address),
            provenOwnDeviceId: transport?.ownDeviceId(address: address)
        )
    }

    /// **§10 step 5, sending.** Push this person's own signed roster at a device
    /// of theirs that has just said hello on a link belonging to them.
    ///
    /// Unrequested, because the device that most needs it is the one that does
    /// not know to ask: a removed phone believes it is still linked, so a
    /// request-shaped exchange would never be started by the only participant who
    /// is wrong. Sent both ways for the same reason, which also settles an
    /// ordinary sibling that is merely behind.
    ///
    /// What it cannot outrun is §10.1's key rotation, which lands at the moment
    /// of removal, long before any meeting: the fleet's own traffic is already
    /// shut to that device when this reaches it. And it hears it from a document
    /// signed under the person root, never from a bare hint. Core refuses to hand
    /// one out at all from a device the gate has silenced, so an ejected phone
    /// does not become an announcer.
    ///
    /// It is not, however, free of disclosure. §10.2's rotation is driven now
    /// (`RelayRotationDriver`), but it lands on the first relay pass that can
    /// reach the relay rather than at the moment of removal — and this notice
    /// crosses only a proven LAN link, which is exactly the situation where
    /// there may be no internet at all. So on a ship it can still tell the
    /// holder of a removed phone the moment they were removed while the shared
    /// relay credential that phone already had is live. Recorded rather than
    /// papered over: the remaining window closes by the rotation landing sooner,
    /// not by withholding the one signal that stops an honest phone believing it
    /// is still linked.
    ///
    /// Mirrors Android's `MeshService.offerOwnRosterNotice`.
    private func offerOwnRosterNotice(address: String, capabilities: UInt32, identity: Identity) {
        guard ownIdentityLinkIsProven(address: address, identity: identity) else { return }
        guard OwnRosterNoticePolicy.peerReadsNotices(peerCapabilities: capabilities) else { return }
        let encoded: Data?
        do {
            // Nil on a device with no roster of its own, and refused outright on
            // one the gate has silenced -- core decides both.
            encoded = try store.ownRosterNoticeFrame()
        } catch {
            log.warning("Could not build our device list for another device of ours")
            return
        }
        guard let frame = encoded else { return }
        // The timer only restarts for a write the router accepted: a send that
        // never left this phone has told the link nothing, and booking it as
        // delivered would sit a half-open own-device link out another full
        // interval.
        guard MeshRouter.sendToAddress(address: address, frame: frame) else { return }
        ownRosterNoticeSchedule.noteOffered(
            address: address,
            nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )
        log.info("Sent our device list to another device of ours on \(address, privacy: .public)")
    }

    /// **§10 step 5, receiving.** A roster of this person's own devices, arriving
    /// from one of them.
    ///
    /// The link test is repeated here and is not a formality: it is what stops a
    /// stranger who claimed our user id in a HELLO from handing us a document at
    /// all. Core then refuses anything the person root did not sign and anything
    /// that does not strictly supersede what this device holds, so the only
    /// document that changes anything here is this person's own, newer than the
    /// one this phone had.
    ///
    /// The outcome worth acting on is `.revokedSelf`: core has already stored the
    /// burying roster, cleared the fleet projection and moved the activation
    /// stage to revoked, which refuses advertising, authoring and acking. What is
    /// left for the shell is the part core cannot see — this phone's own radios,
    /// and the person looking at it.
    ///
    /// Mirrors Android's `MeshService.handleOwnRosterNotice`.
    private func handleOwnRosterNotice(address: String, document: Data, identity: Identity) {
        guard ownIdentityLinkIsProven(address: address, identity: identity) else {
            log.warning("Ignoring a device list from a link that has not proved it is ours")
            return
        }
        guard let ownDeviceId = DeviceKeyStore.load()?.deviceId else {
            // No device key means this install was never part of a roster, so
            // there is nothing a roster could say about it.
            log.info("Ignoring a device list on an install that has never linked")
            return
        }
        let adoption: RevocationAdoption
        do {
            adoption = try store.applyOwnRosterNotice(
                document: document,
                personRootSignPk: identity.signPk,
                ownDeviceId: ownDeviceId,
                nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
            )
        } catch {
            log.warning("Could not read a device list from another device of ours")
            return
        }
        switch adoption.outcome {
        case .revokedSelf:
            log.warning("This device was removed from its person's devices; standing down")
            DeviceRemovalStatus.shared.markRemoved()
            // Takes the radios down through the same path §9.4 uses.
            LinkVisibility.refresh(store: store)
            stopBecauseThisDeviceWasRemoved()
        case .forkQuarantined:
            // DL-2: two signed device lists at the same version. Core
            // deliberately does not eject on one branch of a fork, so this
            // device stays live -- said out loud rather than swallowed, because
            // a quarantined phone that was in fact removed is the one corner
            // where the symptom survives. A surface for it is owed with the rest
            // of §10.4's.
            log.warning("Two different device lists claim the same version; quarantined")
        case .adopted:
            log.info("Took a newer device list from another device of ours")
        default:
            log.info("A device list from \(address, privacy: .public) changed nothing")
        }
    }

    /// A removed device does not keep a mesh running that claims to belong to a
    /// fleet it is no longer in. The preference goes down with the radios so
    /// nothing — a background BLE relaunch, the app coming forward,
    /// `startMeshIfEnabled` — brings them back up behind a screen that says they
    /// are off.
    ///
    /// Written straight to the defaults rather than through `AppModel.stopMesh`
    /// because this runs on `meshQueue` and `AppModel` is main-actor. The root
    /// view calls `stopMesh` as well when it draws `DeviceRemovedView`, which is
    /// what keeps the published copy of the flag honest.
    ///
    /// Mirrors Android's `MeshService.stopBecauseThisDeviceWasRemoved`.
    private func stopBecauseThisDeviceWasRemoved() {
        AppDefaults.current.set(false, forKey: AppModel.meshEnabledKey)
        stopOnMeshQueue()
    }

    private func recordOwnIdentityCloneIfAuthenticated(remoteStaticKey: Data) {
        guard ownLanStaticKeyMatches(ownAgreePk: identity.agreePk, remoteStaticKey: remoteStaticKey) else {
            return
        }
        recordOwnIdentityClone()
    }

    private func recordOwnIdentityClone() {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1_000)
        do {
            try store.recordIdentityCloneWarning(userId: identity.userId, nowMs: nowMs)
            ChatEvents.notifyChatChanged(identity.userId)
        } catch {
            log.warning("Could not record identity clone warning")
        }
        log.warning("Another device presented our identity")
    }

    private func handleHello(address: String, userId: Data, identity: Identity) {
        if noteOwnIdentityHello(address: address, userId: userId, identity: identity) { return }
        let previouslySelectedAddress = MeshRouter.routeFor(userId: userId)?.1
        // Match Android's per-HELLO reaffirmation. Startup ordering already
        // installs this in `configure`, but keeping election input adjacent to
        // HELLO processing prevents a future lifecycle reorder from silently
        // restoring the central-first fallback.
        MeshRouter.setLocalUserId(identity.userId)
        guard MeshRouter.onHello(address: address, userId: userId) else {
            log.warning("Dropping HELLO that conflicts with the authenticated link identity")
            return
        }
        let seenAtMs = Int64(Date().timeIntervalSince1970 * 1_000)
        onMain { MeshConnectivityStatus.shared.mergeLastSeen(userId: userId, seenAtMs: seenAtMs) }
        if (try? store.getContact(userId: userId)) != nil,
           let transport = MeshRouter.transportFor(address: address) {
            recordPeerConnection(userId: userId, transport: transport, kind: .connected)
        }
        log.info("HELLO from \(address, privacy: .public) \(UserIdHex.encode(userId), privacy: .public)")
        let selectedAddress = MeshRouter.routeFor(userId: userId)?.1
        if let selectedAddress,
           selectedAddress != previouslySelectedAddress,
           selectedAddress != address {
            // Reaffirming identity can elect an already-HELLO'd inverse BLE
            // role. Resume there even though this HELLO is on the superseded
            // route.
            resumeLogicalPeerSync(peerUserId: userId)
        }
        guard MeshRouter.isSelectedRoute(address: address) else {
            log.info("HELLO route retained for control/failover; bulk sync uses the elected logical-peer route")
            refreshNearby()
            return
        }
        // Small frames first, and outside every brake: they are the fastest
        // way off this radio entirely if the peer shares our Wi-Fi.
        sendLanEndpointHint(address: address)
        queueCurrentLanEndpoint(to: userId)

        // One whole-encounter engine selection, read once here and not
        // consulted again while this burst runs, so a flip mid-burst cannot
        // split it between two sequencers.
        //
        // Under `.core` everything below — the cadence verdict, the DIGEST,
        // the targeted carry drain, the mule spray and the offer slot the last
        // two share — belongs to `MessageStore.corePlanMeshMeet`. What this
        // shell still owns on that branch is the part that is genuinely
        // transport: the LAN endpoint hints above (control frames, not
        // encounter lanes) and the nearby-route refresh.
        //
        // The default is `.legacy`; every line below this branch is unchanged.
        if MeetEngineSettings.meetEngine() == .core {
            coreMeet.encounter(
                address: address,
                ownUserId: identity.userId,
                peerUserId: userId,
                trigger: .firstContact
            )
            refreshNearby()
            return
        }

        // Cadence gate (#280). This handler claims `.firstContact` because a
        // HELLO is what a fresh encounter looks like from here — two phones
        // meeting and beginning to sync must never be delayed. Core does not
        // take the claim on trust: it downgrades it to reconnect churn from
        // its own record of this peer, which is what makes hundreds of
        // reconnects cost hundreds of map lookups instead of hundreds of
        // bursts.
        let gate = SprayPolicy.maySpray(
            peerUserId: userId,
            address: address,
            trigger: .firstContact
        )
        guard gate.allow else {
            log.info("Holding the HELLO burst for \(address, privacy: .public): retry in \(gate.retryAfterMs)ms")
            rearmGatedSpray(peerUserId: userId, gate: gate)
            refreshNearby()
            return
        }
        // Digest first: it is one small frame, and core's exchange window is
        // measured from the moment it is enqueued. Draining the carry queue
        // ahead of it would put up to a full carried budget into this link's
        // single FIFO first, so the frame would not reach the radio for ~10s
        // and the peer's answer would arrive after the window had shut (#280).
        sendDigest(address: address, userId: userId, identity: identity)
        let drained = drainCarriedEnvelopesTo(
            address: address,
            peerUserId: userId,
            carriedBudgetBytes: gate.carriedBudgetBytes
        )
        SprayPolicy.noteBytesQueued(address: address, bytes: drained)
        refreshNearby()
    }

    /// Re-arm a burst the cadence gate held back, when core says the wait is
    /// short enough to be worth a timer.
    ///
    /// Nothing is dropped either way: past that horizon the ordinary 3-5
    /// minute maintenance tick is the cheaper way back, and it always comes.
    /// The threshold is core's (`SprayPolicy.retryArmMaxMs`) so neither shell
    /// owns it. Re-entry is `scheduleFailoverResume`, the same coalescing path
    /// a dying sibling link uses, so a gate denial landing on top of a live
    /// failover burst still produces one resume rather than two.
    ///
    /// At most one deferral is pending per logical peer, matching Android's
    /// `scheduleDeferredSpray`. Without that, a reconnect storm producing N
    /// HELLOs and N failover resumes to one peer posts N timers, and because
    /// they fire at staggered milliseconds `FailoverResumeDebounce` cannot
    /// collapse them: its window has closed again between each pair. Replacing
    /// the pending item (rather than keeping the first) is what makes the
    /// deferral track the newest denial instead of firing early.
    private func rearmGatedSpray(peerUserId: Data, gate: CoreSprayGate) {
        guard gate.retryWorthArming, gate.retryAfterMs > 0 else { return }
        let key = UserIdHex.encode(peerUserId)
        // A cancelled `DispatchWorkItem` never runs its block, so the replaced
        // timer cannot come back and clear the newer one's map entry. That is
        // what lets the block below remove the key unconditionally.
        pendingSprayDeferrals[key]?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.pendingSprayDeferrals.removeValue(forKey: key)
            guard self.isRunning else { return }
            self.scheduleFailoverResume(peerUserId: peerUserId)
        }
        pendingSprayDeferrals[key] = work
        meshQueue.asyncAfter(
            deadline: .now() + .milliseconds(Int(clamping: gate.retryAfterMs)),
            execute: work
        )
    }

    /// Failover path into `resumeLogicalPeerSync`, delayed and coalesced per
    /// logical peer.
    ///
    /// Running the resume straight out of a disconnect callback is wrong when
    /// several links die in one radio event: the first callback picks whatever
    /// route is *currently* elected — often a sibling link to the same phone
    /// whose own disconnect has not been delivered yet — and immediately
    /// queues a multi-KB carry drain plus a digest onto it, which is then
    /// thrown away when that link dies too. Waiting one
    /// `FailoverResumeDebounce` window lets the rest of the burst land first,
    /// so the resume runs once, against the route that actually survived.
    ///
    /// iOS reaches this from BLE and LAN disconnects the same way Android
    /// does. The hop `meshQueue` already provides is *not* a substitute: it
    /// orders our own callbacks, but the sibling link's disconnect has not
    /// been reported by CoreBluetooth yet, so it is not on the queue to be
    /// ordered against.
    ///
    /// Promotion callers still resume immediately — a promotion means a link
    /// just came *up*, so there is no dying sibling to wait for.
    ///
    /// The window is measured on the same monotonic clock `asyncAfter` counts
    /// down on: on the wall clock, a time correction landing mid-burst would
    /// expire the window early and produce the second fan-out this exists to
    /// prevent.
    private func scheduleFailoverResume(peerUserId: Data) {
        let key = UserIdHex.encode(peerUserId)
        let nowMs = FailoverResumeDebounce.monotonicNowMs
        guard let arm = failoverResumeDebounce.request(key: key, nowMs: nowMs) else { return }
        meshQueue.asyncAfter(deadline: .now() + .milliseconds(Int(clamping: arm.delayMs))) { [weak self] in
            guard let self else { return }
            // Cleared before the work runs, so a disconnect arriving while the
            // resume is in flight arms a fresh window instead of being
            // swallowed by this one. The token scopes that to *this* window: a
            // window armed in the meantime keeps its own timer.
            self.failoverResumeDebounce.fired(key: key, token: arm.token)
            guard self.isRunning else { return }
            self.resumeLogicalPeerSync(peerUserId: peerUserId)
        }
    }

    /// Continue immediately after route promotion or failover; waiting for
    /// periodic maintenance would create a needless LAN/BLE handoff gap.
    private func resumeLogicalPeerSync(peerUserId: Data) {
        guard let identity,
              let route = MeshRouter.routeFor(userId: peerUserId) else { return }
        // The encounter engine selection, same read as `handleHello`. Under
        // `.core` the planner owns the cadence verdict, the digest, the drain
        // and the spray; a peer digest this shell stashed while the gate was
        // shut is handed to it as this encounter's known-id set rather than
        // answered on a separate path, because it is the same encounter.
        if MeetEngineSettings.meetEngine() == .core {
            let gated = takeGatedDigest(peerUserId: peerUserId)
            coreMeet.encounter(
                address: route.1,
                ownUserId: identity.userId,
                peerUserId: peerUserId,
                trigger: .reconnect,
                peerKnownMsgIds: gated?.recentMsgIds ?? [],
                // CARRY-02: replayed unchanged from arrival, never re-derived
                // from the link we happen to answer on.
                peerAuthenticated: gated?.peerAuthenticated ?? false
            )
            return
        }
        // Cadence gate (#280). This is the reconnect-churn path: a link dying
        // and being re-elected several times a minute used to buy a full burst
        // each time. First contact is not this path — `handleHello` owns that
        // — so a denial here only ever delays a repeat.
        let gate = SprayPolicy.maySpray(
            peerUserId: peerUserId,
            address: route.1,
            trigger: .reconnect
        )
        guard gate.allow else {
            log.info("Holding the resume burst for \(route.1, privacy: .public): retry in \(gate.retryAfterMs)ms")
            rearmGatedSpray(peerUserId: peerUserId, gate: gate)
            return
        }
        log.info("Logical peer selected \(route.1, privacy: .public); resuming carry and digest sync")
        // Our own digest is one small frame and it goes FIRST, ahead of every
        // bulk lane below -- see `handleHello` for why the ordering is
        // load-bearing (#280).
        sendDigest(address: route.1, userId: peerUserId, identity: identity)
        // A digest this peer sent while the gate was shut is answered next: its
        // carried-copy confirmations retire envelopes the drain below would
        // otherwise re-offer.
        if let gated = takeGatedDigest(peerUserId: peerUserId) {
            respondToDigest(
                address: route.1,
                peerUserId: peerUserId,
                entries: gated.entries,
                recentMsgIds: gated.recentMsgIds,
                identity: identity,
                gate: gate,
                peerAuthenticated: gated.peerAuthenticated
            )
        }
        let drained = drainCarriedEnvelopesTo(
            address: route.1,
            peerUserId: peerUserId,
            carriedBudgetBytes: gate.carriedBudgetBytes
        )
        SprayPolicy.noteBytesQueued(address: route.1, bytes: drained)
    }

    /// A peer DIGEST held back by the cadence gate, waiting for a replay.
    ///
    /// Android keeps the same map for the same reason (`MeshService.kt`'s
    /// `gatedDigests`). A refused digest must be held, not discarded: this is
    /// the only path that sends the receipts we owe that peer and the 1:1
    /// backlog its watermark asked for, and receiving our own digest does not
    /// make a peer send another -- so dropping it stalls both until that peer's
    /// own 3-5 minute maintenance tick. It also carries `recentMsgIds`, which
    /// is what `coreConfirmCarriedDeliveries` needs to retire carried rows the
    /// peer just proved it holds; discarding those leaves the rows in our carry
    /// store to be re-offered. #241 is what a stuck receipt watermark costs.
    ///
    /// `peerAuthenticated` is captured at ARRIVAL (CARRY-02) and replayed
    /// unchanged so a BLE-sourced digest can never be treated as authenticated
    /// merely because it is answered on a later-elected LAN link.
    private struct GatedDigest {
        let entries: [DigestEntry]
        let recentMsgIds: [Data]
        let peerAuthenticated: Bool
    }

    private func stashGatedDigest(
        peerUserId: Data,
        entries: [DigestEntry],
        recentMsgIds: [Data],
        peerAuthenticated: Bool
    ) {
        gatedDigests[UserIdHex.encode(peerUserId)] = GatedDigest(
            entries: entries,
            recentMsgIds: recentMsgIds,
            peerAuthenticated: peerAuthenticated
        )
    }

    private func takeGatedDigest(peerUserId: Data) -> GatedDigest? {
        gatedDigests.removeValue(forKey: UserIdHex.encode(peerUserId))
    }

    /// Encode and send the §7.3 digest for `address` and record the time so
    /// `checkDigestMaintenance` can re-run it on a long-lived link (D8). Called
    /// at HELLO time and on the periodic re-digest tick.
    private func sendDigest(address: String, userId: Data, identity: Identity) {
        let entries: [DigestEntry]
        if let contact = try? store.getContact(userId: userId) {
            entries = (try? store.chatDigest(chatId: contact.userId)) ?? []
        } else {
            entries = []
        }
        // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): the advertised list
        // now includes not just what we're still carrying for others but
        // also what we've recently consumed or authored ourselves, so a
        // mule still holding our envelope learns on this digest that we
        // already have it -- see `store.coreConfirmCarriedDeliveries`.
        let advertised = (try? store.coreDigestAdvertisedMsgIds()) ?? []
        guard let digest = try? encodeDigest(
            chatId: identity.userId,
            entries: entries,
            recentMsgIds: advertised
        ) else {
            log.warning("Could not encode DIGEST for \(address, privacy: .public)")
            return
        }
        MeshRouter.sendToAddress(address: address, frame: digest)
        SprayPolicy.noteDigestSent(peerUserId: userId, address: address)
        sendGroupDigests(address: address, userId: userId, identity: identity)
    }

    private func sendGroupDigests(address: String, userId: Data, identity: Identity) {
        let advertised = (try? store.coreDigestAdvertisedMsgIds()) ?? []
        let groups = (try? store.listGroups()) ?? []
        for group in groups where group.memberUserIds.contains(userId)
            && group.memberUserIds.contains(identity.userId) {
            let entries = (try? store.chatDigest(chatId: group.id)) ?? []
            guard let digest = try? encodeDigest(
                chatId: group.id,
                entries: entries,
                recentMsgIds: advertised
            ) else { continue }
            MeshRouter.sendToAddress(address: address, frame: digest)
        }
    }

    private func handleGroupDigest(
        address: String,
        chatId: Data,
        entries: [DigestEntry],
        peerUserId: Data?,
        identity: Identity
    ) {
        guard let peerUserId,
              let group = try? store.getGroup(groupId: chatId),
              digestIsSharedGroup(
                digestChatId: chatId,
                helloUserId: peerUserId,
                ownUserId: identity.userId,
                group: group
              ) else {
            log.warning("Dropping DIGEST from \(address, privacy: .public)")
            return
        }
        let gate = SprayPolicy.maySpray(
            peerUserId: peerUserId,
            address: address,
            trigger: .peerDigest
        )
        guard gate.allow else {
            log.info("Skipping group DIGEST for \(address, privacy: .public)")
            return
        }
        let peerHasThrough = DigestSync.throughLamportForSelf(entries: entries, ownUserId: identity.userId)
        var queuedBytes = resendGroupOutboundToPeer(
            address: address,
            peerUserId: peerUserId,
            identity: identity,
            afterLamport: peerHasThrough,
            onlyGroupId: group.id
        )
        if let contact = try? store.getContact(userId: peerUserId) {
            queuedBytes += syncGroupReceiptsToPeer(identity: identity, contact: contact, address: address)
        }
        SprayPolicy.noteBytesQueued(address: address, bytes: queuedBytes)
        groupDigestAnswers.insert(groupDigestAnswerKey(peerUserId: peerUserId, groupId: group.id))
    }

    /// Battery, 2026-07-21: foreground-only (see `setAppForeground`). This
    /// tick only *re-sends* our own digest on links idle past their jittered
    /// re-digest window (`checkDigestMaintenance` -> `sendDigest`) -- a
    /// convergence nudge for messages/receipts that arrived after the
    /// connect-time digest, not the delivery path itself. Actual envelope
    /// delivery over BLE (and LAN) is driven directly by received
    /// `.envelope` frames in `onFrameReceived` -> `processInboundEnvelope`,
    /// which fires from CoreBluetooth/Network.framework callbacks
    /// independent of this timer -- see that function. `handleHello` also
    /// already sends a fresh digest on every (re)connect, so a background
    /// BLE link that drops and reconnects still gets one on the reconnect;
    /// only an already-long-lived link's periodic *re*-digest is deferred
    /// until the app returns to foreground. Nothing correctness-critical for
    /// background BLE frame relay depends on this tick.
    ///
    /// Invalidates and returns (no timer) when backgrounded; fires an
    /// immediate catch-up check before arming the repeating timer whenever
    /// this runs while foregrounded, so a foreground return doesn't wait out
    /// a stale 60s -- semantics on links that are actually due are otherwise
    /// unchanged from before this gating.
    ///
    /// The tick fires on `meshQueue`, so its digest sends are ordered against
    /// received frames exactly like every other mesh event -- a re-digest can
    /// never land halfway through an envelope being processed.
    private func startDigestMaintenanceLoop() {
        digestMaintenanceTimer?.cancel()
        digestMaintenanceTimer = nil
        guard appForeground else { return }
        checkDigestMaintenance()
        digestMaintenanceTimer = meshTimer(intervalSeconds: 60, repeats: true) { [weak self] in
            self?.checkDigestMaintenance()
        }
    }

    /// D8: re-run the digest exchange on links that have stayed up past their
    /// jittered 3-5 min interval so a message/receipt that arrived after the
    /// connect-time digest still converges without a reconnect. Digests are
    /// idempotent, so over-calling is safe.
    private func checkDigestMaintenance() {
        guard let identity else { return }
        let routes = MeshRouter.selectedIdentifiedRoutes()
        let now = SprayPolicy.nowMs
        for route in routes {
            // Core still owns the jittered 3-5 minute window; what is new is
            // that a link whose sprays keep producing no receipt progress
            // waits longer, and that this tick and the event-driven callers
            // now read the same record instead of one writing and the other
            // reading. No bookkeeping is retained here: core prunes its own.
            let gate = SprayPolicy.maySpray(
                peerUserId: route.userId,
                address: route.address,
                trigger: .maintenance,
                nowMs: now
            )
            if gate.allow {
                sendDigest(address: route.address, userId: route.userId, identity: identity)
            }
        }
    }

    private func handleDigest(
        address: String,
        chatId: Data,
        entries: [DigestEntry],
        recentMsgIds: [Data],
        identity: Identity
    ) {
        let peerUserId = MeshRouter.userIdFor(address: address)
        guard DigestSync.isExpectedChatId(digestChatId: chatId, helloUserId: peerUserId),
              let peerUserId else {
            handleGroupDigest(
                address: address,
                chatId: chatId,
                entries: entries,
                peerUserId: peerUserId,
                identity: identity
            )
            return
        }
        // CARRY-02: the authentication of a carried-delivery confirmation must
        // bind to the transport the digest ARRIVED on, not the link we happen
        // to answer on. It is captured here, at arrival, and carried unchanged
        // through any stash and replay; deriving it later from the elected
        // route would let a digest that arrived over unauthenticated Bluetooth
        // be answered on a freshly-elected LAN link and have its advertised ids
        // laundered into an authenticated removal. A `.lan` transport is filed
        // only after a completed Noise handshake whose static key matched an
        // accepted contact (`lan.onAuthenticated`); a Bluetooth link is not.
        let peerAuthenticated = MeshRouter.transportFor(address: address) == .lan
        // Cadence gate (#280). This is the larger outbound half of the
        // exchange, so leaving it ungated would brake nothing. It is normally
        // allowed: our own just-sent digest opened core's exchange window, and
        // this digest is the answer to it. What it refuses is an unprovoked
        // digest arriving from a peer reconnecting every few seconds.
        let gate = SprayPolicy.maySpray(
            peerUserId: peerUserId,
            address: address,
            trigger: .peerDigest
        )
        guard gate.allow else {
            log.info("Holding the digest response for \(address, privacy: .public): retry in \(gate.retryAfterMs)ms")
            // Held, not discarded -- see `GatedDigest`. `resumeLogicalPeerSync`
            // replays it, which is what `rearmGatedSpray` schedules.
            stashGatedDigest(
                peerUserId: peerUserId,
                entries: entries,
                recentMsgIds: recentMsgIds,
                peerAuthenticated: peerAuthenticated
            )
            rearmGatedSpray(peerUserId: peerUserId, gate: gate)
            return
        }
        respondToDigest(
            address: address,
            peerUserId: peerUserId,
            entries: entries,
            recentMsgIds: recentMsgIds,
            identity: identity,
            gate: gate,
            peerAuthenticated: peerAuthenticated
        )
    }

    /// The outbound half of `handleDigest`, split out so a digest the cadence
    /// gate held back can be replayed unchanged once the gate opens (see
    /// `GatedDigest`). Android's `MeshService.respondToDigest` is the twin.
    ///
    /// `address` is passed rather than re-derived: on the replay path the
    /// elected route may have moved since the digest arrived, and what the peer
    /// told us about its own state is true whichever link we answer on.
    ///
    /// `peerAuthenticated` is likewise passed, not re-derived: it must reflect
    /// the transport the digest ARRIVED on (CARRY-02), which on the replay path
    /// may differ from `address`'s current transport.
    private func respondToDigest(
        address: String,
        peerUserId: Data,
        entries: [DigestEntry],
        recentMsgIds: [Data],
        identity: Identity,
        gate: CoreSprayGate,
        peerAuthenticated: Bool
    ) {
        // Everything queued here that is not part of the spray plan -- the
        // receipt repair pass, the per-missing-message re-send loop and the
        // group catch-up -- is counted and charged against this link's burst
        // allowance below. They are the encounter's LARGEST lanes, and while
        // they went uncharged a second DIGEST arriving inside the exchange
        // window could re-run all of them against an untouched allowance
        // (#280).
        var queuedBytes = 0
        if let contact = try? store.getContact(userId: peerUserId) {
            queuedBytes += syncReceiptsFirst(identity: identity, contact: contact, address: address)
            queuedBytes += syncGroupReceiptsToPeer(identity: identity, contact: contact, address: address)
            let peerHasThrough = DigestSync.throughLamportForSelf(entries: entries, ownUserId: identity.userId)
            let queued = (try? store.outboundEnvelopesAfter(
                chatId: contact.userId,
                senderUserId: identity.userId,
                afterLamport: peerHasThrough
            )) ?? []
            let byLamport = Dictionary(uniqueKeysWithValues: queued.map { ($0.lamport, $0) })
            // Same once-per-session bound as the core spray plan, and asked
            // the same way -- per kind. A peer that lacks the bit for a kind
            // never advances its DELIVERED watermark past that kind, so this
            // direct re-offer would repeat it on every digest for the full
            // expiry; a kind the peer does advertise is untouched.
            let alreadyOffered = Set(MeshRouter.hiddenOfferedFor(address: address))
            var newlyOffered: [Data] = []
            let missing = (try? store.messagesAfter(
                chatId: contact.userId,
                senderUserId: identity.userId,
                afterLamport: peerHasThrough
            )) ?? []
            for message in missing {
                let outbound = byLamport[message.lamport]
                    ?? backfillOutbound(identity: identity, contact: contact, message: message)
                if let outbound {
                    if coreIsHiddenSprayKind(kind: outbound.kind),
                       !MeshRouter.peerAcksHiddenKind(address: address, kind: outbound.kind) {
                        if alreadyOffered.contains(outbound.msgId) { continue }
                        newlyOffered.append(outbound.msgId)
                    }
                    if MeshRouter.sendToAddress(address: address, frame: encodeOutboundEnvelopeFrame(outbound)) {
                        queuedBytes += outbound.sealed.count
                    }
                }
            }
            MeshRouter.recordHiddenOffered(address: address, msgIds: newlyOffered)
        }
        queuedBytes += resendGroupOutboundToPeer(
            address: address,
            peerUserId: peerUserId,
            identity: identity,
            afterLamport: 0,
            skipAnsweredGroups: true
        )
        // Charged before the plan is built, so `sprayDigestPlanTo`'s own
        // admission sees a link allowance that already reflects what this
        // encounter has queued.
        SprayPolicy.noteBytesQueued(address: address, bytes: queuedBytes)
        // The encounter engine selection. The lanes above — receipt repair,
        // the per-missing-message re-send and the group catch-up — are the
        // digest *answer* and stay with this shell on both branches; what the
        // branch selects is who plans the offer half of the encounter: the
        // core planner's digest-confirm, targeted drain and budgeted spray, or
        // this shell's own spray path.
        if MeetEngineSettings.meetEngine() == .core {
            coreMeet.encounter(
                address: address,
                ownUserId: identity.userId,
                peerUserId: peerUserId,
                trigger: .peerDigest,
                peerKnownMsgIds: recentMsgIds,
                peerAuthenticated: peerAuthenticated
            )
            return
        }
        sprayDigestPlanTo(
            address: address,
            peerUserId: peerUserId,
            peerKnownIds: recentMsgIds,
            identity: identity,
            gate: gate,
            peerAuthenticated: peerAuthenticated
        )
    }

    /// DTN D4 (seen-set poisoning ordering, mirrors Android
    /// `MeshService.processInboundEnvelope`'s KDoc): [GossipState.seenIds]
    /// is checked with the non-mutating `contains`, never `checkAndRecord`,
    /// and only recorded once this envelope reaches a **terminal handled
    /// state** -- consumed, carried, or expired-drop -- at each `return`
    /// below. Invariant: an envelope whose durable handling failed must be
    /// re-presentable; an envelope that was handled (even by deliberate
    /// drop) must be deduped. Before this, `checkAndRecord` ran up front, so
    /// a later store failure (e.g. disk-full out of `carryForeign`)
    /// permanently poisoned the `msgId` even though it was never actually
    /// carried or delivered.
    ///
    /// Loop-hazard note (see `relayForeign`'s doc comment): recording after
    /// relaying is safe here because the arriving link is excluded from the
    /// relay fanout and this function runs synchronously per received frame,
    /// so this node cannot re-ingest the frame it just relayed before the
    /// `record` call below completes.
    ///
    /// "Synchronously per received frame" is now enforced by `meshQueue`, the
    /// controller's serial queue (see the class doc): every frame, connect,
    /// disconnect and timer tick runs in one queue block, and this function
    /// contains no suspension point, so no second envelope's processing can
    /// begin between the gate above and the `record` at the end. The relay
    /// pass calls in through `onMeshQueue`, so its envelopes serialise here
    /// with BLE/LAN frames rather than racing them.
    private func processInboundEnvelope(
        sourceAddress: String?,
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data,
        identity: Identity
    ) -> CoreInboundDisposition {
        // Whole-envelope engine selection, read once here and not consulted
        // again while this frame is handled, so a flip cannot mix engines
        // within one envelope. Default legacy: everything below this line is
        // unchanged until the internal switch is turned on.
        if InboundEngineSettings.pathEngine() == .core {
            return processInboundEnvelopeViaCore(
                sourceAddress: sourceAddress,
                msgId: msgId,
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed,
                identity: identity
            )
        }
        let sourceLabel = sourceAddress ?? "relay"
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        switch coreInboundGate(
            isNewMsgId: !GossipState.seenIds.contains(msgId: msgId),
            hopTtl: hopTtl,
            expiryMs: expiry,
            nowMs: now
        ) {
        case .seen:
            return .seen
        case .expired:
            log.info("Dropping expired envelope from \(sourceLabel, privacy: .public)")
            // A deliberate drop is still a terminal handled state.
            GossipState.seenIds.record(msgId: msgId)
            return .expired
        case .rejected:
            log.warning("Dropping envelope with invalid hop or expiry fields from \(sourceLabel, privacy: .public)")
            GossipState.seenIds.record(msgId: msgId)
            return .rejected
        case .dispatch:
            break
        }
        let opened: OpenedMessage
        do {
            opened = try openMessage(recipient: identity, sealed: sealed)
        } catch {
            // Pairwise open failed: either foreign 1:1 traffic, or a group
            // envelope sealed with a shared key (DESIGN.md §6.5). Try groups
            // whose recipient_hint matches before treating it as pure mule
            // traffic. Group members keep relaying/carrying so absent members
            // still get a copy.
            //
            // T4-06: this catch is deliberately scoped to `openMessage` ONLY.
            // A store failure while delivering a message that WAS ours must
            // not be misread here as "not for us, carry as foreign" -- the
            // own-delivery path below has its own catch that returns .failed.
            if let (group, opened) = tryOpenGroupMessage(
                recipientHint: recipientHint, ownUserId: identity.userId, sealed: sealed, now: now
            ) {
                do {
                    try deliverOpenedGroupEnvelope(
                        sourceLabel: sourceLabel,
                        group: group,
                        opened: opened,
                        identity: identity,
                        msgId: msgId,
                        arrival: messageArrival(
                            sourceAddress: sourceAddress,
                            senderUserId: opened.senderUserId,
                            receivedHopTtl: hopTtl
                        )
                    )
                } catch {
                    // T4-06: durable store of our own group copy failed. Leave
                    // re-presentable (no record) and never acked.
                    log.warning("Deferring group envelope from \(sourceLabel, privacy: .public): durable delivery failed")
                    return .failed
                }
                // specs/group-relay-durability.md §4.3 no-reinjection rule:
                // a relay-fetched group message addressed to OUR OWN hint is
                // a per-member fan-out copy -- the relay fan-out already
                // reaches every member durably, so re-flooding/carrying it
                // would give the same content a second flood identity under
                // the fan-out msgId. Legacy group-hint relay rows and every
                // BLE/LAN-sourced group frame keep the flood+carry behavior.
                // Mirrors MeshService.kt.
                let ownFanoutCopy = sourceAddress == nil &&
                    coreIsOwnFanoutHint(
                        recipientHint: recipientHint,
                        ownUserId: identity.userId,
                        nowMs: now
                    )
                if !ownFanoutCopy {
                    relayForeign(
                        sourceAddress: sourceAddress,
                        msgId: msgId,
                        hopTtl: hopTtl,
                        expiry: expiry,
                        recipientHint: recipientHint,
                        sealed: sealed
                    )
                    _ = carryForeign(
                        msgId: msgId,
                        hopTtl: hopTtl,
                        expiry: expiry,
                        recipientHint: recipientHint,
                        sealed: sealed,
                        forceFamily: true
                    )
                }
                // DTN D4: we already durably delivered our own copy above
                // (`deliverOpenedGroupEnvelope`), so record regardless of
                // whether the best-effort mule copy for absent members
                // succeeded -- same reasoning as Android's KDoc.
                GossipState.seenIds.record(msgId: msgId)
                return .consumed
            }
            relayForeign(
                sourceAddress: sourceAddress,
                msgId: msgId,
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed
            )
            let carried = carryForeign(
                msgId: msgId,
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed
            )
            // DTN D4: only record once the durable carry actually succeeded.
            // `carryForeign` reports store failure via its Bool return
            // (rather than swallowing it silently), so a disk-full failure
            // here leaves this msgId unrecorded: the next copy of this
            // envelope on any link re-gates as `.dispatch` and gets another
            // chance to carry it, instead of being silently dropped as
            // `.seen` for the rest of the process lifetime.
            if carried {
                GossipState.seenIds.record(msgId: msgId)
            }
            return .carried
        }

        // `openMessage` succeeded: this envelope is ours. Delivering it is a
        // separate do/catch from the open above so a store failure here is
        // reported as `.failed` (re-presentable, never acked) rather than
        // being mistaken for foreign traffic (T4-06).
        let arrival = messageArrival(
            sourceAddress: sourceAddress,
            senderUserId: opened.senderUserId,
            receivedHopTtl: hopTtl
        )
        let consumed: PairwiseDeliveryResult?
        do {
            consumed = try deliverOpened(
                sourceLabel: sourceLabel,
                sourceAddress: sourceAddress,
                opened: opened,
                identity: identity,
                msgId: msgId,
                arrival: arrival
            )
        } catch {
            log.warning("Deferring envelope from \(sourceLabel, privacy: .public): durable delivery failed")
            return .failed
        }
        // The ONE place this device may vouch for a hidden kind's relay copy:
        // reaching here means `openMessage` succeeded against our own identity
        // key (so the envelope was pairwise-sealed to us and nobody else can
        // open it) and delivery ran to completion. Both consumption paths pass
        // through here -- BLE/LAN frames and relay-fetched envelopes alike --
        // so a relay-consumed hidden kind is equally re-ackable if the mailbox
        // ever re-presents it. Mirrors InboundEnvelopeProcessor.kt; see
        // `coreRecordConsumedHiddenMsgId` for every condition core re-checks
        // and for why anything unprovable must not be recorded.
        if let consumed {
            recordConsumedHiddenKind(
                msgId: msgId,
                consumed: consumed,
                recipientHint: recipientHint,
                expiry: expiry,
                identity: identity,
                now: now
            )
        }
        // DTN D4: delivery ran to completion -- safe, and required, to record.
        GossipState.seenIds.record(msgId: msgId)
        return .consumed
    }

    /// The same envelope, dispositioned by core's one inbound transaction
    /// (`MessageStore.processInboundFrame`) instead of by the function above.
    ///
    /// What moved: the dedupe/expiry/header gate, the pairwise and group opens,
    /// the signer-is-a-member guard, the blocked-sender drop, the carry, the
    /// hop decrement, the relay no-reinjection rule, and every `seen` record
    /// -- all of it now decided once, in core, inside store transactions that
    /// have committed before the call returns. What stayed: this class remains
    /// the driver. It picks the links a re-flood goes out on, applies the
    /// delivered payload through the same per-kind handlers as before, and
    /// commits afterwards.
    ///
    /// The order is the production `deliver -> commit` order, and it is the
    /// whole reason core hands a commit token back rather than recording the
    /// bookkeeping itself: if the native delivery below throws, the token is
    /// dropped, the `msgId` stays unrecorded and re-presentable, no
    /// consumed-hidden evidence is written, and this reports `.failed` -- so
    /// the relay copy is never acked away for a message that never landed
    /// (T4-06 / DTN D4).
    ///
    /// Runs on `meshQueue` like its legacy twin -- the caller is unchanged --
    /// so the FFI call below is off the main actor. That is load-bearing
    /// rather than incidental: a receive pipeline doing core work on the main
    /// actor is the shape of freeze this app has already shipped once.
    private func processInboundEnvelopeViaCore(
        sourceAddress: String?,
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data,
        identity: Identity
    ) -> CoreInboundDisposition {
        let sourceLabel = sourceAddress ?? "relay"
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        // Core takes the §6.4 frame, not the parsed fields, because the parse
        // and the frame limits are part of the disposition it owns. Re-encoding
        // what `parseFrame` just produced is byte-identical by construction.
        let frame = encodeEnvelopeFrame(
            msgId: msgId,
            hopTtl: hopTtl,
            expiry: expiry,
            recipientHint: recipientHint,
            sealed: sealed
        )
        let outcome: CoreInboundOutcome
        do {
            outcome = try store.processInboundFrame(
                identity: identity,
                seen: GossipState.seenIds,
                source: InboundAdapter.source(forSourceAddress: sourceAddress),
                frame: frame,
                nowMs: now
            )
        } catch {
            // A store failure inside the transaction. Core records nothing seen
            // on that path, so this envelope stays re-presentable on its next
            // copy and its relay row is never acked.
            log.warning("Deferring envelope from \(sourceLabel, privacy: .public): inbound transaction failed")
            return .failed
        }
        let plan = InboundAdapter.plan(from: outcome)

        // Execution, in the order the legacy path used. Whether there is a
        // frame to flood, and what its hop count is, was decided in core; this
        // only chooses the links -- excluding the arriving one, which is the
        // echo guard the legacy path has always had.
        if let relayFrame = plan.relayFrame {
            if let sourceAddress {
                _ = MeshRouter.relayToAllExcept(sourceAddress, frame: relayFrame)
            } else {
                _ = MeshRouter.relayToAll(frame: relayFrame)
            }
        }
        // A mesh-carried envelope addressed to someone this device knows is
        // worth waking the relay pass for, exactly as `carryForeign` did. The
        // family test is core's own `hintMatchesKnownTarget` -- the shell reads
        // the answer, it does not decide what "family" means -- and the
        // relay-source half of the condition is core's own carry rule, so a
        // relay-fetched row never wakes a pass to re-upload itself. See
        // `InboundExecutionPlan.wakesRelayPass`.
        if plan.wakesRelayPass(
            source: InboundAdapter.source(forSourceAddress: sourceAddress),
            hintIsKnownTarget: (try? store.hintMatchesKnownTarget(hint: recipientHint, nowMs: now)) == true
        ) {
            RelaySyncEvents.requestSync()
        }
        if plan.droppedBlocked {
            log.info("Dropping envelope from blocked sender on \(sourceLabel, privacy: .public)")
        }

        guard let delivery = plan.delivery else { return plan.disposition }
        let consumed: PairwiseDeliveryResult?
        do {
            consumed = try applyCoreDeliveredPayload(
                sourceLabel: sourceLabel,
                sourceAddress: sourceAddress,
                delivery: delivery,
                identity: identity,
                msgId: msgId,
                hopTtl: hopTtl
            )
        } catch {
            log.warning("Deferring envelope from \(sourceLabel, privacy: .public): durable delivery failed")
            return .failed
        }
        // Delivery landed. Commit what core deferred: the ACK-01 hidden-kind
        // evidence (best-effort, every safety condition re-checked in the
        // store) and the DTN D4 `seen` record.
        store.coreCommitInboundDelivery(seen: GossipState.seenIds, commit: delivery.commit)
        if let consumed {
            recordConsumedStreamLamport(consumed)
        }
        return plan.disposition
    }

    /// Applies one payload core decided this device should receive, through the
    /// same handlers the legacy path calls.
    ///
    /// Core has already proved the envelope was ours to open, that a group
    /// body's signer and this device are both current members, and that the
    /// sender is not blocked; those checks are not repeated as policy here. The
    /// handlers below re-derive what they need about the body itself -- the
    /// 1:1 stream rule that `chatId` is the sender, the group rule that
    /// `chatId` is the group -- because that is the body's own validity, which
    /// is where each handler has always enforced it.
    ///
    /// Pairwise and group are told apart by `commit.hiddenKind`, which core
    /// populates only on the pairwise-open path (recording hidden-kind ack
    /// evidence is a pairwise-only licence) and leaves `nil` for a group
    /// delivery or a body that did not decode. Using it here is deliberate and
    /// worth stating, because the alternative -- routing on whether `chatId`
    /// names a known group -- would let a 1:1-sealed body claiming a group's
    /// id reach the group handlers, which the legacy path drops outright.
    private func applyCoreDeliveredPayload(
        sourceLabel: String,
        sourceAddress: String?,
        delivery: (payload: Data, senderUserId: Data, commit: CoreInboundCommit),
        identity: Identity,
        msgId: Data,
        hopTtl: UInt8
    ) throws -> PairwiseDeliveryResult? {
        let opened = OpenedMessage(senderUserId: delivery.senderUserId, payload: delivery.payload)
        let arrival = messageArrival(
            sourceAddress: sourceAddress,
            senderUserId: delivery.senderUserId,
            receivedHopTtl: hopTtl
        )
        if delivery.commit.hiddenKind != nil {
            return try deliverOpened(
                sourceLabel: sourceLabel,
                sourceAddress: sourceAddress,
                opened: opened,
                identity: identity,
                msgId: msgId,
                arrival: arrival
            )
        }
        guard let extendedBody = try? decodeExtendedMessageBody(bytes: delivery.payload) else {
            // Undecodable body from a verified sender: a deterministic reject,
            // and a terminal handled state rather than a store failure.
            return nil
        }
        guard let group = try? store.getGroup(groupId: extendedBody.chatId) else {
            log.info("Dropping group envelope from \(sourceLabel, privacy: .public): no imported group for its chat id")
            return nil
        }
        try deliverOpenedGroupEnvelope(
            sourceLabel: sourceLabel,
            group: group,
            opened: opened,
            identity: identity,
            msgId: msgId,
            arrival: arrival
        )
        // Group deliveries record no hidden-kind evidence and no 1:1 stream
        // lamport; both are pairwise-only.
        return nil
    }

    /// Best-effort note that this device consumed this envelope as its sole
    /// true endpoint consumer, so a later relay copy of the same `msgId` can
    /// be acked away instead of sitting in the mailbox until expiry.
    ///
    /// Deliberately swallows store failures: a missing record costs one relay
    /// re-fetch, which is precisely the cost this mechanism trades against,
    /// and must never turn into a failed delivery. Core owns every safety
    /// condition (kind, own-hint, group-hint, expiry) and simply declines to
    /// write a row when one doesn't hold, so this call site's only job is to
    /// be reached exclusively from the proven-consumption path above.
    ///
    /// The same terminal hook records an exact, validated pairwise lamport for
    /// gap rendering when the handler left no message row. Core accepts that
    /// longer-lived evidence only for an established contact and an actual 1:1
    /// stream, so stranger onboarding traffic cannot grow it indefinitely.
    private func recordConsumedHiddenKind(
        msgId: Data,
        consumed: PairwiseDeliveryResult,
        recipientHint: Data,
        expiry: Int64,
        identity: Identity,
        now: Int64
    ) {
        _ = try? store.coreRecordConsumedHiddenMsgId(
            msgId: msgId,
            kind: consumed.kind,
            recipientHint: recipientHint,
            expiryMs: expiry,
            ownUserId: identity.userId,
            nowMs: now
        )
        recordConsumedStreamLamport(consumed)
    }

    /// Records an exact, validated pairwise lamport for gap rendering when the
    /// handler left no message row. Core accepts that longer-lived evidence
    /// only for an established contact and an actual 1:1 stream, so stranger
    /// onboarding traffic cannot grow it indefinitely.
    ///
    /// Shared by both inbound engines: the core path's hidden-kind evidence is
    /// written by `coreCommitInboundDelivery`, but this gap-rendering record is
    /// not part of that commit, so it is called separately there.
    private func recordConsumedStreamLamport(_ consumed: PairwiseDeliveryResult) {
        guard consumed.recordStreamLamport else { return }
        if (try? store.recordConsumedHiddenLamport(
            chatId: consumed.senderUserId,
            senderUserId: consumed.senderUserId,
            lamport: consumed.lamport,
            kind: consumed.kind
        )) == true {
            ChatEvents.notifyChatChanged(consumed.senderUserId)
        }
    }

    private func messageArrival(
        sourceAddress: String?,
        senderUserId: Data,
        receivedHopTtl: UInt8
    ) -> MessageArrival {
        let transport: UInt8
        if let sourceAddress {
            let linkPeerMatchesSender = MeshRouter.userIdFor(address: sourceAddress) == senderUserId
            if MeshRouter.transportFor(address: sourceAddress) == .lan {
                transport = linkPeerMatchesSender ? 3 : 4
            } else {
                transport = linkPeerMatchesSender ? 0 : 1
            }
        } else {
            transport = 2
        }
        let hopsTaken = arrivalHopsTaken(receivedHopTtl: receivedHopTtl)
        return MessageArrival(
            transport: transport,
            hopsTaken: hopsTaken,
            receivedAt: Int64(Date().timeIntervalSince1970 * 1_000)
        )
    }

    /// Opens `sealed` with any imported group `groupOpenCandidates` offers
    /// for `recipientHint`: groups whose own recent-day hints match, plus
    /// every imported group when the hint is OUR OWN -- a per-member relay
    /// fan-out copy (specs/group-relay-durability.md §4.1) is addressed to
    /// the member, not the group, so nothing but the group key identifies
    /// it. Returns the matching group and opened payload, or nil.
    /// `openGroupMessage` does not check membership of the signer; callers
    /// must enforce that before trusting the body. Mirrors
    /// InboundEnvelopeProcessor.kt.
    private func tryOpenGroupMessage(
        recipientHint: Data,
        ownUserId: Data,
        sealed: Data,
        now: Int64
    ) -> (Group, OpenedMessage)? {
        let groups = (try? store.groupOpenCandidates(
            hint: recipientHint, ownUserId: ownUserId, nowMs: now
        )) ?? []
        for group in groups {
            if let opened = try? openGroupMessage(group: group, sealed: sealed) {
                return (group, opened)
            }
        }
        return nil
    }

    private struct PairwiseDeliveryResult {
        let kind: UInt8
        let senderUserId: Data
        let lamport: UInt64
        /// Invalid/unauthorized stream metadata remains terminally consumed
        /// for relay-ack purposes but cannot close a legitimate chat gap.
        let recordStreamLamport: Bool
    }

    /// Returns the body's stream metadata once it is known, or `nil` if the
    /// body could not even be decoded. Every other early return still reports
    /// its kind for relay-ack evidence, but marks invalid/unauthorized stream
    /// metadata as unable to close a chat gap:
    /// a deliberate discard (blocked sender, unauthorized sender, unhandled
    /// kind) is consumption by an endpoint that is finished with the envelope,
    /// which is exactly what `processInboundEnvelope` treats as `.consumed`
    /// and what may be recorded as a consumed hidden kind. Only "we could not
    /// tell what this was" withholds that. Mirrors
    /// InboundEnvelopeProcessor.kt's `deliverOpenedEnvelope`.
    @discardableResult
    private func deliverOpened(
        sourceLabel: String,
        sourceAddress: String?,
        opened: OpenedMessage,
        identity: Identity,
        msgId: Data,
        arrival: MessageArrival
    ) throws -> PairwiseDeliveryResult? {
        let extendedBody: ExtendedMessageBody
        do {
            extendedBody = try decodeExtendedMessageBody(bytes: opened.payload)
        } catch {
            // Undecodable body from a verified sender: deterministic reject,
            // terminal handled state (not a store failure).
            return nil
        }
        let body = MessageBody(
            kind: extendedBody.kind,
            chatId: extendedBody.chatId,
            lamport: extendedBody.lamport,
            timestamp: extendedBody.timestamp,
            content: extendedBody.content
        )
        guard body.chatId == opened.senderUserId else {
            return PairwiseDeliveryResult(
                kind: body.kind,
                senderUserId: opened.senderUserId,
                lamport: body.lamport,
                recordStreamLamport: false
            )
        }
        let senderIsContact = (try? store.getContact(userId: opened.senderUserId)) != nil
        guard corePairwiseSenderAuthorized(
            kind: body.kind,
            senderIsContact: senderIsContact,
            senderIsSelf: opened.senderUserId == identity.userId
        ) else {
            log.warning("Dropping pairwise envelope from unauthorized sender on \(sourceLabel, privacy: .public)")
            return PairwiseDeliveryResult(
                kind: body.kind,
                senderUserId: opened.senderUserId,
                lamport: body.lamport,
                recordStreamLamport: false
            )
        }

        // Blocked identities are dropped before ANY kind handler runs: a
        // replayed kind=3 must not resurrect the contact, no receipts are
        // authored (the blocked party sees nothing), and the relay copy still
        // acks away as consumed — we are the sole endpoint and deliberate
        // discard is consumption, so the mailbox doesn't refetch it forever.
        if (try? store.isUserBlocked(userId: opened.senderUserId)) == true {
            log.info("Dropping envelope from blocked sender on \(sourceLabel, privacy: .public)")
            return PairwiseDeliveryResult(
                kind: body.kind,
                senderUserId: opened.senderUserId,
                lamport: body.lamport,
                recordStreamLamport: false
            )
        }

        switch body.kind {
        case ProtocolKind.text, ProtocolKind.attachmentManifest, ProtocolKind.reaction:
            try handleIncomingChat(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity,
                kind: body.kind,
                msgId: msgId,
                replyToMsgId: extendedBody.replyToMsgId,
                senderDeviceId: extendedBody.senderDeviceId,
                arrival: arrival
            )
        case ProtocolKind.receipt:
            try handleIncomingReceipt(
                sourceAddress: sourceAddress,
                envelopeSender: opened.senderUserId,
                body: body,
                identity: identity,
                arrival: arrival
            )
        case ProtocolKind.friendRequest:
            try handleIncomingFriendRequest(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.profileSync:
            try handleIncomingProfileSync(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.friendDirectory:
            try handleIncomingFriendDirectory(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.introducedFriendRequest:
            try handleIncomingIntroducedFriendRequest(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.lanEndpointHint:
            try handleIncomingLanEndpointHint(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.relayUpdate:
            try handleIncomingRelayUpdate(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.rosterGossip:
            try handleIncomingRosterGossip(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity,
                senderDeviceId: extendedBody.senderDeviceId
            )
        case ProtocolKind.groupInvite:
            try handleIncomingGroupInvite(
                sourceLabel: sourceLabel,
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        default:
            log.info("Unhandled kind=\(body.kind) from \(sourceLabel, privacy: .public)")
        }
        return PairwiseDeliveryResult(
            kind: body.kind,
            senderUserId: opened.senderUserId,
            lamport: body.lamport,
            recordStreamLamport: true
        )
    }

    /// Delivers a group-sealed envelope we opened with an imported group key
    /// (DESIGN.md §6.5). Wire `MessageBody.chatId` is the group id; the
    /// verified signer must be a current member (core does not check this).
    /// Group receipts are deferred — we only store + notify.
    private func deliverOpenedGroupEnvelope(
        sourceLabel: String,
        group: Group,
        opened: OpenedMessage,
        identity: Identity,
        msgId: Data,
        arrival: MessageArrival?
    ) throws {
        guard group.memberUserIds.contains(opened.senderUserId) else {
            log.warning("Dropping group envelope from \(sourceLabel, privacy: .public): signer is not a member of \(group.name, privacy: .public)")
            return
        }
        guard group.memberUserIds.contains(identity.userId) else {
            log.warning("Dropping group envelope from \(sourceLabel, privacy: .public): we are not a member of \(group.name, privacy: .public)")
            return
        }
        let extendedBody: ExtendedMessageBody
        do {
            extendedBody = try decodeExtendedMessageBody(bytes: opened.payload)
        } catch {
            return
        }
        let body = MessageBody(
            kind: extendedBody.kind,
            chatId: extendedBody.chatId,
            lamport: extendedBody.lamport,
            timestamp: extendedBody.timestamp,
            content: extendedBody.content
        )
        guard body.chatId == group.id else {
            log.warning("Dropping group envelope from \(sourceLabel, privacy: .public): body.chatId does not match group id")
            return
        }
        if (try? store.isUserBlocked(userId: opened.senderUserId)) == true {
            log.info("Dropping group envelope from blocked sender on \(sourceLabel, privacy: .public)")
            return
        }
        switch body.kind {
        case ProtocolKind.text, ProtocolKind.attachmentManifest, ProtocolKind.reaction:
            try handleIncomingGroupChatMessage(
                group: group,
                senderUserId: opened.senderUserId,
                body: body,
                msgId: msgId,
                replyToMsgId: extendedBody.replyToMsgId,
                senderDeviceId: extendedBody.senderDeviceId,
                arrival: arrival
            )
        case ProtocolKind.groupMetadataUpdate:
            try handleIncomingGroupMetadataUpdate(
                sourceLabel: sourceLabel,
                group: group,
                senderUserId: opened.senderUserId,
                body: body,
                msgId: msgId,
                replyToMsgId: extendedBody.replyToMsgId,
                senderDeviceId: extendedBody.senderDeviceId,
                arrival: arrival
            )
        default:
            log.info("Dropping group envelope from \(sourceLabel, privacy: .public): unhandled kind=\(body.kind)")
        }
    }

    private func acceptIncomingInsert(
        _ outcome: IncomingMessageInsertOutcome,
        sourceLabel: String,
        kind: UInt8,
        lamport: UInt64,
        senderUserId: Data
    ) -> Bool {
        switch outcome {
        case .inserted:
            return true
        case .duplicate:
            log.info(
                "Ignoring duplicate kind=\(kind, privacy: .public) lamport=\(lamport, privacy: .public) on \(sourceLabel, privacy: .public)"
            )
            return false
        case .quarantinedConflict:
            log.warning(
                "Quarantined message stream conflict kind=\(kind, privacy: .public) lamport=\(lamport, privacy: .public) on \(sourceLabel, privacy: .public); retained visible branch"
            )
            ChatEvents.notifyChatChanged(senderUserId)
            return false
        }
    }

    private func handleIncomingGroupMetadataUpdate(
        sourceLabel: String,
        group: Group,
        senderUserId: Data,
        body: MessageBody,
        msgId: Data,
        replyToMsgId: Data?,
        senderDeviceId: Data?,
        arrival: MessageArrival?
    ) throws {
        let updated: Group?
        do {
            let update = try decodeGroupMetadataUpdate(bytes: body.content)
            updated = try applyGroupMetadataUpdate(
                group: group,
                update: update,
                senderUserId: senderUserId
            )
        } catch {
            // Deterministic reject (bad/inapplicable metadata) -- terminal
            // handled state, distinct from a store failure. Swallow here so
            // it is NOT reported as .failed by the caller.
            log.warning("Dropping invalid group metadata from \(sourceLabel, privacy: .public)")
            return
        }
        // T4-06: primary store failure propagates (see handleIncomingChat).
        let message = StoredMessage(
            chatId: group.id,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: body.kind,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        )
        // A nil `arrival` is a carry-queue drain, where no live transport can
        // truthfully be attributed to the original arrival; the device-aware
        // insert takes that as an optional rather than needing a second entry
        // point.
        let outcome = try store.insertIncomingMessageFromDevice(
            message: message,
            senderDeviceId: senderDeviceId,
            msgId: msgId,
            replyToMsgId: replyToMsgId,
            arrival: arrival
        )
        guard acceptIncomingInsert(
            outcome,
            sourceLabel: sourceLabel,
            kind: body.kind,
            lamport: body.lamport,
            senderUserId: senderUserId
        ) else { return }
        if let updated {
            do {
                try store.upsertGroup(group: updated)
                ChatEvents.notifyChatChanged(group.id)
            } catch {
                log.error("Failed to persist group metadata revision \(updated.metadataRevision)")
            }
        }
    }

    private func handleIncomingGroupChatMessage(
        group: Group,
        senderUserId: Data,
        body: MessageBody,
        msgId: Data,
        replyToMsgId: Data?,
        senderDeviceId: Data?,
        arrival: MessageArrival?
    ) throws {
        // T4-06: primary store failure propagates (see handleIncomingChat).
        let message = StoredMessage(
            chatId: group.id,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: body.kind,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        )
        // See [handleIncomingGroupMetadataUpdate] for why `arrival` stays
        // optional here.
        let outcome = try store.insertIncomingMessageFromDevice(
            message: message,
            senderDeviceId: senderDeviceId,
            msgId: msgId,
            replyToMsgId: replyToMsgId,
            arrival: arrival
        )
        guard acceptIncomingInsert(
            outcome,
            sourceLabel: "group transport",
            kind: body.kind,
            lamport: body.lamport,
            senderUserId: senderUserId
        ) else { return }
        recordInboundChatArrival(senderUserId: senderUserId, kind: body.kind, arrival: arrival)
        ChatEvents.notifyChatChanged(group.id)

        let throughLamport = PeerStreamWatermark.through(
            store: store,
            chatId: group.id,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: group.id,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: throughLamport
        )
        let chatVisible = ChatVisibility.isVisible(group.id)
        if chatVisible {
            try? store.recordOutgoingReceipt(
                chatId: group.id,
                senderUserId: senderUserId,
                receiptType: ReceiptType.read,
                throughLamport: throughLamport
            )
        }
        if let identity {
            emitGroupReceiptsToAuthor(group: group, authorUserId: senderUserId, identity: identity)
        }
        incomingAnnouncements.announceGroupIfNeeded(
            chatVisible: chatVisible,
            kind: body.kind,
            group: group,
            senderName: {
                (try? self.store.getContact(userId: senderUserId))
                    .map { coreContactDisplayName(contact: $0) }
                    ?? String(UserIdHex.encode(senderUserId).prefix(8))
            },
            preview: {
                body.kind == ProtocolKind.attachmentManifest
                    ? AttachmentPayload.previewLabel(AttachmentPayload.decode(body.content))
                    : (String(data: body.content, encoding: .utf8) ?? "")
            }
        )
    }

    /// Imports a pairwise-sealed `kind=4` group invite (DESIGN.md §6.5). Wire
    /// `chatId` is the invite sender's userId (1:1 pairwise convention); the
    /// group id/key/members live in the invite content. Local history is stored
    /// under `chat_id = group.id`.
    private func handleIncomingGroupInvite(
        sourceLabel: String,
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        let group: Group
        do {
            group = try decodeGroupInviteContent(bytes: body.content)
        } catch {
            log.warning("Dropping group invite from \(sourceLabel, privacy: .public): failed to decode")
            return
        }
        guard group.memberUserIds.contains(identity.userId) else {
            log.warning("Dropping group invite from \(sourceLabel, privacy: .public): we are not listed as a member")
            return
        }
        guard group.memberUserIds.contains(senderUserId) else {
            log.warning("Dropping group invite from \(sourceLabel, privacy: .public): sender is not listed as a member")
            return
        }

        // T4-06: persisting the group is the durable state that matters --
        // let a store failure propagate so the invite is not acked/deduped
        // and the group is not silently lost (previously this returned, which
        // the caller treated as consumed and acked the relay copy away).
        try store.upsertGroup(group: group)
        deliverCarriedMessagesForImportedGroup(group: group, identity: identity)
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: group.id,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.groupInvite,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        ))
        guard inserted else { return }
        ChatEvents.notifyChatChanged(group.id)
        log.info("Imported group \(group.name, privacy: .public) from invite on \(sourceLabel, privacy: .public)")

        // The invite rides the 1:1 pairwise lamport stream, so it must be
        // acknowledged on that stream like any other pairwise kind -- even
        // though its row lives under the group chat. Skipping the ack (as this
        // did) strands the peer's delivered watermark below the invite's
        // lamport for as long as the invite is the newest thing they sent us,
        // and the repair lane can never lift it, so they replay their backlog
        // on every send. DELIVERED only, like every other row that never
        // appears in the 1:1 chat.
        if let contact = try? store.getContact(userId: senderUserId) {
            acknowledgeHiddenMessage(
                sourceAddress: sourceAddress,
                senderUserId: senderUserId,
                identity: identity,
                contact: contact,
                atLeastLamport: body.lamport
            )
        }

        incomingAnnouncements.announceGroupInviteIfNeeded(
            chatVisible: ChatVisibility.isVisible(group.id),
            group: group
        )
    }

    private func handleIncomingChat(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity,
        kind: UInt8,
        msgId: Data,
        replyToMsgId: Data?,
        senderDeviceId: Data?,
        arrival: MessageArrival
    ) throws {
        // T4-06: let a store failure propagate (do NOT `try?`-swallow it into
        // the same `false` a harmless duplicate returns). `processInboundEnvelope`
        // turns the throw into `.failed`, leaving the envelope re-presentable
        // and its relay copy un-acked. A `false` here is a real duplicate --
        // already durably stored -- so it stays a terminal (return) state.
        // `senderDeviceId` is whatever the sealed body named as its authoring
        // device (§5). A legacy peer names nothing, and core maps that absence
        // onto the reserved legacy stream -- the one this path has always
        // written to -- so nil is expected here, not missing data.
        let outcome = try store.insertIncomingMessageFromDevice(
            message: StoredMessage(
                chatId: senderUserId,
                senderUserId: senderUserId,
                lamport: body.lamport,
                timestamp: body.timestamp,
                kind: kind,
                payload: body.content,
                senderDeviceId: coreLegacyDeviceId()
            ),
            senderDeviceId: senderDeviceId,
            msgId: msgId,
            replyToMsgId: replyToMsgId,
            arrival: arrival
        )
        guard acceptIncomingInsert(
            outcome,
            sourceLabel: sourceAddress ?? "relay",
            kind: kind,
            lamport: body.lamport,
            senderUserId: senderUserId
        ) else { return }
        ChatEvents.notifyChatChanged(senderUserId)

        let through = PeerStreamWatermark.through(
            store: store,
            chatId: senderUserId,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: senderUserId,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: through
        )
        let visible = ChatVisibility.isVisible(senderUserId)
        if visible {
            try? store.recordOutgoingReceipt(
                chatId: senderUserId,
                senderUserId: senderUserId,
                receiptType: ReceiptType.read,
                throughLamport: through
            )
        }
        guard let contact = try? store.getContact(userId: senderUserId) else { return }
        recordInboundChatArrival(senderUserId: senderUserId, kind: kind, arrival: arrival)
        _ = queueOutgoingReceiptForRelay(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.delivered,
            ackedSenderUserId: senderUserId,
            throughLamport: through
        )
        if visible {
            _ = queueOutgoingReceiptForRelay(
                identity: identity,
                contact: contact,
                receiptType: ReceiptType.read,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        }
        if let sourceAddress {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: sourceAddress,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
            if visible {
                sendReceiptOnAddress(
                    identity: identity,
                    contact: contact,
                    address: sourceAddress,
                    receiptType: ReceiptType.read,
                    ackedSenderUserId: senderUserId,
                    throughLamport: through
                )
            }
        } else {
            sendReceiptToContact(
                identity: identity,
                contact: contact,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
            if visible {
                sendReceiptToContact(
                    identity: identity,
                    contact: contact,
                    receiptType: ReceiptType.read,
                    ackedSenderUserId: senderUserId,
                    throughLamport: through
                )
            }
        }
        incomingAnnouncements.announceDirectIfNeeded(
            chatVisible: visible,
            kind: kind,
            contact: contact,
            preview: {
                if kind == ProtocolKind.attachmentManifest {
                    return AttachmentPayload.previewLabel(AttachmentPayload.decode(body.content))
                }
                return String(data: body.content, encoding: .utf8) ?? ""
            }
        )
        RelaySyncEvents.requestSync()
    }

    private func handleIncomingReceipt(
        sourceAddress: String?,
        envelopeSender: Data,
        body: MessageBody,
        identity: Identity,
        arrival: MessageArrival
    ) throws {
        guard let receipt = try? decodeReceiptContent(bytes: body.content) else { return }
        if let groupId = receipt.groupId {
            try handleIncomingGroupReceipt(
                sourceAddress: sourceAddress,
                envelopeSender: envelopeSender,
                receipt: receipt,
                groupId: groupId,
                identity: identity,
                arrival: arrival
            )
            return
        }
        guard receipt.senderUserId == identity.userId else { return }
        guard (try? store.getContact(userId: envelopeSender)) != nil else { return }
        // A receipt is the other half of what the receipt-quiet backoff (#280)
        // watches for: sprays toward this peer are converging, so its cadence
        // returns to normal.
        SprayPolicy.noteReceiptProgress(peerUserId: envelopeSender)
        // T4-06: advancing the receipt watermark is the durable state here;
        // let a store failure propagate so a relay-fetched receipt is not
        // acked away before it is recorded. T6: the receipt returned on the
        // exact link that delivered the message -- record that route against
        // the watermark so every acked message's Info pane can prove the
        // BLE/LAN/relay round trip, not just the one at the watermark lamport.
        // `receivedAtMs` also lets core record the .messageDelivered half of
        // the Connection details evidence, which is now the only place it is
        // recorded. This shell used to write that event itself on every
        // delivered receipt, which made the screen claim a friend had received
        // a message when all their phone had acked was a profile-sync or
        // friend-directory blob. Core records it only when the watermark newly
        // covers a visible message we authored.
        try store.recordReceipt(
            chatId: envelopeSender,
            senderUserId: identity.userId,
            receiptType: receipt.receiptType,
            throughLamport: receipt.lamport,
            viaTransport: arrival.transport,
            receivedAtMs: arrival.receivedAt
        )
        // V2 field metric: stamp delivery latency + route on the messages this
        // cumulative delivery receipt confirms.
        if receipt.receiptType == ReceiptType.delivered {
            try? store.recordDeliveredMetric(
                chatId: envelopeSender,
                throughLamport: receipt.lamport,
                deliveredAtMs: arrival.receivedAt,
                viaTransport: arrival.transport
            )
        }
        ChatEvents.notifyChatChanged(envelopeSender)
    }

    // FI5: throws now (was fully swallowed) -- matches the T4-06 discipline
    // already used by handleIncomingChat/handleIncomingReceipt/
    // handleIncomingGroupInvite in this file. `deliverOpened`'s catch turns
    // this into `.failed`, leaving the relay copy un-acked and the envelope
    // re-presentable, instead of a transient store failure (disk-full, busy)
    // permanently destroying a friend request. The two store writes below
    // that matter for that guarantee -- `upsertImportedContact` (durably
    // adds the contact; the actual effect of "processing a friend request")
    // and `insertMessage` (the dedup gate, same shape as every other
    // handler here) -- now propagate; everything else (provenance,
    // suggestion cleanup, outbound profile-sync queueing, receipts) stays
    // best-effort `try?`, same as before.
    /// Was this peer in range when we accepted them? Recorded in
    /// `ContactProvenance.addedNearby` so the composer can stay quiet about
    /// nearby-only delivery for people we actually met, and say it plainly for
    /// people who only ever arrived over the internet.
    ///
    /// Read from `MeshRouter` rather than from `MeshConnectivityStatus`, which
    /// is main-actor state this pipeline can no longer read synchronously.
    /// Not a change of answer: `MeshConnectivityStatus.nearbyPeerIds` is
    /// derived from exactly this list by `refreshNearbyRoutes`, and asking the
    /// router directly is if anything the fresher of the two, since the
    /// published copy trails by one hop to the main actor.
    private func peerIsNearby(_ userId: Data) -> Bool {
        MeshRouter.identifiedRoutes().contains { $0.userId == userId }
    }

    private func handleIncomingFriendRequest(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        let pending = ((try? store.listFriendSuggestions(
            nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )) ?? []).first { $0.state == 1 && $0.candidate.userId == senderUserId }
        // Deterministic reject: undecodable/mismatched friend card from a
        // verified sender. Not a store failure -- stays a swallowed terminal
        // state.
        guard let json = String(data: body.content, encoding: .utf8),
              let content = try? parseFriendRequestContent(json: json),
              friendCardUserId(card: content.card) == senderUserId else { return }
        // A request carrying a shared-card tail is never auto-imported: it
        // waits until this person says yes (specs/share-contact.md decision 5).
        // A tailless one is a direct scan and keeps today's behaviour, forever.
        if let shared = content.shared {
            try handleSharedFriendRequest(
                senderUserId: senderUserId,
                card: content.card,
                shared: shared,
                identity: identity
            )
            return
        }
        let card = content.card
        let wasKnown = (try? store.getContact(userId: senderUserId)) != nil
        let contact = Contact(
            userId: senderUserId,
            name: card.name,
            signPk: card.signPk,
            agreePk: card.agreePk,
            relayUrl: card.relayUrl,
            relayToken: card.relayToken
        )
        _ = try store.upsertImportedContact(contact: contact)
        // Their plain request answers ours: whatever we were waiting on from a
        // shared code has now completed, so stop saying "waiting" about it.
        try? store.deleteOutgoingSharedRequest(candidateUserId: senderUserId)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }
        try? store.upsertContactProvenance(provenance: ContactProvenance(
            userId: senderUserId,
            source: pending == nil ? 0 : 1,
            introducerUserId: pending?.introducerUserId,
            introducedAtMs: Int64(Date().timeIntervalSince1970 * 1_000),
            addedNearby: peerIsNearby(senderUserId)
        ))
        if pending != nil { try? store.removeFriendSuggestion(candidateUserId: senderUserId) }
        ProfileSyncSender.queueToContact(
            store: store,
            identity: identity,
            contact: contact,
            displayName: ProfileStore.loadDisplayName(),
            epoch: ProfileStore.loadOwnAvatarEpoch()
        )
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.friendRequest,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        ))
        guard inserted else { return }
        ChatEvents.notifyChatChanged(senderUserId)
        let through = PeerStreamWatermark.through(
            store: store,
            chatId: senderUserId,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: senderUserId,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: through
        )
        if let sourceAddress {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: sourceAddress,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        }
        if !wasKnown {
            FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
            FriendImportEvents.notify(FriendImportEvent(contact: contact, directBluetooth: sourceAddress != nil))
            incomingAnnouncements.announceFriendAdded(contact: contact)
        }
        log.info("Imported contact \(UserIdHex.encode(contact.userId), privacy: .public) from friend request")
    }

    /// A `kind=3` that came out of somebody's **Share contact** code
    /// (specs/share-contact.md). Nothing is written to `contacts` or to chat
    /// history here: the request waits in `pending_shared_requests` until this
    /// person answers it, and every check below drops it with no prompt at all.
    /// Silence is the point -- a prompt for a request that failed verification
    /// would be the surface an attacker was after.
    ///
    /// Returning normally consumes the relay copy, exactly as the tailless path
    /// does; no delivered receipt is authored, because nothing was stored to
    /// report a watermark through and a receipt would claim a delivery the user
    /// has not agreed to. `upsertPendingSharedRequest` is the one write that
    /// must not be silently lost (FI5), so it propagates. No LAN endpoint hint
    /// goes back either: this phone advertises its endpoint to contacts, and
    /// the requester is deliberately not one yet.
    private func handleSharedFriendRequest(
        senderUserId: Data,
        card: FriendCard,
        shared: SharedFriendCard,
        identity: Identity
    ) throws {
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let dismissal = (try? store.getSharedRequestDismissal(requesterUserId: senderUserId)) ?? nil
        guard (try? store.isUserBlocked(userId: senderUserId)) != true,
              dismissal?.suppressed != true,
              friendCardUserId(card: shared.card) == identity.userId,
              FriendsOfFriendsStore.isEnabled(),
              let sharer = try? store.getContact(userId: shared.sharerUserId),
              (try? store.isUserBlocked(userId: sharer.userId)) != true,
              (try? verifySharedFriendCard(
                shared: shared,
                sharerSignPk: sharer.signPk,
                expectedCardUserId: identity.userId,
                expectedPolicyRevision: FriendsOfFriendsStore.revision(),
                nowMs: now
              )) == true else { return }

        // A redelivery updates the waiting row rather than stacking a second
        // prompt; the requester's own card supplies how to reach them if and
        // when this person accepts.
        try store.upsertPendingSharedRequest(request: PendingSharedRequest(
            requesterUserId: senderUserId,
            name: card.name,
            signPk: card.signPk,
            agreePk: card.agreePk,
            relayUrl: card.relayUrl,
            relayToken: card.relayToken,
            sharerUserId: shared.sharerUserId,
            expiresAtMs: shared.expiresAtMs,
            firstSeenMs: now,
            lastPromptedMs: 0
        ))
        ChatEvents.notifyChatChanged(senderUserId)
        // At most one prompt per requester per day (core keeps the clock), so a
        // resend cannot be used to wear somebody down.
        if (try? store.noteSharedRequestPrompt(requesterUserId: senderUserId, nowMs: now)) == true {
            incomingAnnouncements.announceSharedRequest(name: card.name, userId: senderUserId)
        }
        log.info("Holding a shared-card friend request for confirmation from \(UserIdHex.encode(sharer.userId), privacy: .public)")
    }

    // FI5: throws now -- see handleIncomingFriendRequest's doc for the
    // general rationale. `insertMessage` is the only MessageStore write in
    // this handler (the LAN endpoint cache/connect calls above it are a
    // separate, best-effort local cache, not the durable record this
    // envelope's ack decision is gated on), so it's the primary write here.
    private func handleIncomingLanEndpointHint(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: unknown sender or undecodable hint. Not a
        // store failure -- stays a swallowed terminal state.
        guard let contact = try? store.getContact(userId: senderUserId),
              let hint = try? decodeLanEndpointContent(bytes: body.content),
              let networkId = String(data: hint.networkId, encoding: .utf8) else { return }
        let endpoint = LanManualEndpoint(host: hint.host, port: hint.port)
        LanCapabilityStore.markSupported(userId: senderUserId)
        refreshLanCapableContacts()
        queueCurrentLanEndpoint(to: senderUserId)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }

        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        // A sealed hint is only good for its stated lifetime (fifteen
        // minutes; see LanEndpointSender). This envelope may have sat in a
        // relay backlog for hours or days, so an expired hint is neither
        // saved nor dialed -- saving first would re-file a long-dead address
        // and reset its seven-day cache clock on every replay.
        if hint.expiresAtMs > now {
            LanEndpointCache.save(
                networkId: networkId,
                userId: senderUserId,
                endpoint: endpoint,
                provenance: .hinted
            )
            // The network fingerprint is stored with the cached endpoint but
            // deliberately does NOT gate this dial: requiring an exact match
            // silently disabled fresh hints on routed multi-subnet LANs --
            // the case the sealed hint exists for (mDNS is link-local; TCP
            // may still route). A cross-network false positive is one
            // bounded TCP attempt to an endpoint the contact sealed to us,
            // and Noise authenticates. Being on some Wi-Fi is the only
            // requirement.
            if currentLanNetworkId != nil {
                lanTransport?.connect(endpoint, remoteInstanceToken: hint.instanceToken)
            }
        }

        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.lanEndpointHint,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        ))
        if inserted {
            acknowledgeHiddenMessage(
                sourceAddress: sourceAddress,
                senderUserId: senderUserId,
                identity: identity,
                contact: contact
            )
        }
    }

    /// T23: a contact told us their own relay endpoint changed.
    ///
    /// `senderUserId` is the identity core verified sealed this envelope, and
    /// it is the only user id passed to `applyContactRelayUpdate` — core
    /// rejects the notice outright if its payload claims a different subject,
    /// so a notice can only ever move its own sender's endpoint, never a third
    /// party's. `insertMessage` is the dedup gate, same shape as every other
    /// handler in this file.
    private func handleIncomingRelayUpdate(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: unknown sender or undecodable content. Not a
        // store failure -- stays a swallowed terminal state.
        guard let contact = try? store.getContact(userId: senderUserId),
              let content = try? decodeRelayUpdateContent(bytes: body.content) else { return }
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.relayUpdate,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        ))
        guard inserted else { return }

        // A mis-scoped subject or a non-deposit credential throws: a
        // deterministic reject, not a store failure. The message row above
        // still stands so the sender's watermark advances and they stop
        // re-spraying it.
        // The clock is passed so core can refuse an epoch further ahead than
        // believable skew -- a notice stamped past the end of time would
        // otherwise pin this contact's endpoint shut forever.
        let applied = (try? store.applyContactRelayUpdate(
            senderUserId: senderUserId,
            content: content,
            nowMs: Int64(Date().timeIntervalSince1970 * 1000)
        )) ?? false
        if applied {
            log.info("Applied a relay update from \(UserIdHex.encode(contact.userId), privacy: .public)")
            // Anything queued for them was addressed to the old endpoint's
            // mailbox; a sync pass re-resolves and re-posts to the new one.
            RelaySyncEvents.requestSync()
            ChatEvents.notifyChatChanged(senderUserId)
        }
        acknowledgeHiddenMessage(
            sourceAddress: sourceAddress,
            senderUserId: senderUserId,
            identity: identity,
            contact: contact
        )
    }

    /// DL-3's receive side: a contact's own device list, arriving as ordinary
    /// sealed 1:1 mail (`specs/multi-device-v1.md` §4, §9 step 5, §10.1).
    ///
    /// Shaped exactly like `handleIncomingRelayUpdate`, and for the same reasons:
    /// the hidden row goes in first so this contact's DELIVERED watermark
    /// advances past a document they would otherwise re-offer for a week, and the
    /// decision itself is core's single `applyContactRoster` funnel — DL-1
    /// ordering, DL-2 fork quarantine, DL-4 tombstones and §10.4's
    /// changed-safety facts, all decided in one place. "Refused" is a recorded
    /// outcome here, not an error.
    ///
    /// # The one check that is duplicated, and when it stops being
    ///
    /// `rosterGossipDescribesSender` restates a rule core already enforces in
    /// `deliver_inbound_body`'s `KIND_ROSTER_GOSSIP` arm
    /// (`core/src/session/mesh_receive.rs`). It is repeated here only because
    /// this shell has not yet moved its per-kind delivery onto
    /// `core_deliver_inbound`; the moment it does, this handler and the check
    /// with it are deleted, not maintained. Android carries the identical
    /// mirrored handler, and the two go together.
    ///
    /// It cannot simply be dropped in the meantime. `applyContactRoster` takes a
    /// document and no sender, and verifies it against the person the document
    /// names — so without this, a contact could hand us a *genuine* roster about
    /// a third party, and a stale one at that. A stale roster is exactly the
    /// document that still vouches for a device its person has since buried.
    private func handleIncomingRosterGossip(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity,
        senderDeviceId: Data?
    ) throws {
        guard let contact = try? store.getContact(userId: senderUserId) else {
            log.info("Dropping device list from \(sourceAddress ?? "a relay copy", privacy: .public): sender is not a contact")
            return
        }
        guard let roster = try? coreDecodeRoster(bytes: body.content) else {
            log.warning("Dropping device list: it could not be read")
            return
        }
        guard rosterGossipDescribesSender(
            rosterPersonId: roster.personId,
            senderUserId: senderUserId
        ) else {
            log.warning("Dropping device list: it is not about the sender")
            return
        }
        // The row is filed so this contact's DELIVERED watermark advances past
        // the document. A duplicate row is *not* a reason to skip the apply
        // below: `applyContactRoster` is idempotent by DL-1 (a roster that is
        // not newer than the one held is a recorded no-op).
        _ = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.rosterGossip,
            payload: body.content,
            senderDeviceId: senderDeviceId ?? coreLegacyDeviceId()
        ))

        // A store failure, not a refusal: refusals come back as an outcome. The
        // row above still stands, so the sender's watermark advances and they
        // stop re-spraying the same document.
        if let decision = try? store.applyContactRoster(incoming: roster) {
            log.info(
                "Device list from \(UserIdHex.encode(senderUserId), privacy: .public): \(String(describing: decision.outcome), privacy: .public) (\(String(describing: decision.reason), privacy: .public))"
            )
        } else {
            log.warning("Could not apply the device list from \(UserIdHex.encode(senderUserId), privacy: .public)")
        }
        // The chat itself gained a hidden row, and §10.4's banner is read from
        // the store when a chat opens, so nudge whatever is on screen.
        ChatEvents.notifyChatChanged(senderUserId)
        // Ack, exactly as `handleIncomingRelayUpdate` does. Without this the
        // sender's DELIVERED watermark never moves past the roster, so they
        // re-offer the same document on every digest for the whole life of the
        // envelope -- the ACK-MD-2 churn this carrier exists to end.
        acknowledgeHiddenMessage(
            sourceAddress: sourceAddress,
            senderUserId: senderUserId,
            identity: identity,
            contact: contact
        )
    }

    // FI5: throws now -- see handleIncomingFriendRequest's doc for the
    // general rationale. `insertMessage` is the dedup gate here, same shape
    // as every other handler in this file; the profile-content writes below
    // it (avatar/name/policy) stay best-effort `try?`, unchanged.
    private func handleIncomingProfileSync(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: unknown sender or undecodable content. Not a
        // store failure -- stays a swallowed terminal state.
        guard let existing = try? store.getContact(userId: senderUserId),
              let content = try? decodeProfileSyncContent(bytes: body.content) else { return }
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.profileSync,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        ))
        guard inserted else { return }

        let policyChanged = (try? store.upsertContactDiscoveryPolicy(policy: ContactDiscoveryPolicy(
            userId: senderUserId,
            protocolVersion: content.friendsOfFriendsVersion,
            enabled: content.friendsOfFriendsEnabled,
            revision: content.friendsOfFriendsRevision
        ))) ?? false

        let applied = (try? store.setContactAvatar(
            userId: senderUserId,
            avatar: content.avatar.isEmpty ? nil : content.avatar,
            epoch: content.avatarEpoch
        )) ?? false
        if applied, content.name != existing.name {
            let updated = Contact(
                userId: existing.userId,
                name: content.name,
                signPk: existing.signPk,
                agreePk: existing.agreePk,
                relayUrl: existing.relayUrl,
                relayToken: existing.relayToken
            )
            try? store.upsertContact(contact: updated)
        }
        ChatEvents.notifyChatChanged(senderUserId)

        let contact = (try? store.getContact(userId: senderUserId)) ?? existing
        let through = PeerStreamWatermark.through(
            store: store,
            chatId: senderUserId,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: senderUserId,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: through
        )
        _ = queueOutgoingReceiptForRelay(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.delivered,
            ackedSenderUserId: senderUserId,
            throughLamport: through
        )
        if let sourceAddress {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: sourceAddress,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        } else {
            sendReceiptToContact(
                identity: identity,
                contact: contact,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        }
        if policyChanged {
            FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
        }
        RelaySyncEvents.requestSync()
    }

    // FI5: throws now -- see handleIncomingFriendRequest's doc for the
    // general rationale. `insertMessage` is the dedup gate here, same shape
    // as every other handler in this file. `applyFriendDirectory` below
    // stays `try?` deliberately: its throw conflates a store failure with a
    // deterministic fail-closed ticket-check reject (see its doc comment),
    // and reclassifying an unrecoverable ticket rejection as `.failed` would
    // make it retry forever instead of being a swallowed terminal state.
    private func handleIncomingFriendDirectory(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: unknown sender or undecodable content. Not a
        // store failure -- stays a swallowed terminal state.
        guard let contact = try? store.getContact(userId: senderUserId),
              let content = try? decodeFriendDirectoryContent(bytes: body.content) else { return }
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.friendDirectory,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        ))
        guard inserted else { return }
        if FriendsOfFriendsStore.isEnabled() {
            // Introductions stay inside one Shore Pass. A directory from an
            // introducer on somebody else's pass is applied as an *empty*
            // snapshot rather than ignored: the revision bookkeeping stays
            // identical, and it additionally clears whatever that introducer
            // supplied before this rule existed. A phone therefore heals on
            // its own next pass instead of waiting for every other phone in
            // the graph to update. Mirrors InboundEnvelopeProcessor.kt.
            var scoped = content
            if !FriendDirectoryScope.introducible(contact, ownRelay: RelayConfigStore.load()) {
                log.info("Scoping out friend directory: introducer is on another pass")
                scoped = FriendDirectoryContent(
                    version: content.version,
                    revision: content.revision,
                    entries: []
                )
            }
            guard (try? store.applyFriendDirectory(
                introducerUserId: senderUserId,
                recipientUserId: identity.userId,
                content: scoped,
                nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
            )) != nil else { return }
            ChatEvents.notifyChatChanged(senderUserId)
        }
        acknowledgeHiddenMessage(
            sourceAddress: sourceAddress,
            senderUserId: senderUserId,
            identity: identity,
            contact: contact
        )
    }

    // FI5: throws now -- see handleIncomingFriendRequest's doc for the
    // general rationale; same two primary writes (`upsertImportedContact`,
    // `insertMessage`) propagate here for the same reason.
    private func handleIncomingIntroducedFriendRequest(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: feature disabled, undecodable request, or a
        // ticket that fails verification (fail-closed by design -- an
        // invalid/expired/mismatched ticket should never be retried into
        // success). Not a store failure -- stays a swallowed terminal state.
        guard FriendsOfFriendsStore.isEnabled(),
              let request = try? decodeIntroducedFriendRequest(bytes: body.content),
              let card = try? parseFriendCard(json: request.friendCardJson),
              friendCardUserId(card: card) == senderUserId,
              let introducer = try? store.getContact(userId: request.ticket.introducerUserId),
              (try? verifyIntroductionTicket(
                ticket: request.ticket,
                introducerSignPk: introducer.signPk,
                expectedCandidateUserId: identity.userId,
                expectedInviteeUserId: senderUserId,
                expectedCandidatePolicyRevision: FriendsOfFriendsStore.revision(),
                nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
              )) == true else { return }

        let wasKnown = (try? store.getContact(userId: senderUserId)) != nil
        let contact = Contact(
            userId: senderUserId,
            name: card.name,
            signPk: card.signPk,
            agreePk: card.agreePk,
            relayUrl: card.relayUrl,
            relayToken: card.relayToken
        )
        _ = try store.upsertImportedContact(contact: contact)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }
        try? store.upsertContactProvenance(provenance: ContactProvenance(
            userId: senderUserId,
            source: 1,
            introducerUserId: introducer.userId,
            introducedAtMs: Int64(Date().timeIntervalSince1970 * 1_000),
            addedNearby: peerIsNearby(senderUserId)
        ))
        try? store.removeFriendSuggestion(candidateUserId: senderUserId)
        _ = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.introducedFriendRequest,
            payload: body.content,
            senderDeviceId: coreLegacyDeviceId()
        ))
        acknowledgeHiddenMessage(
            sourceAddress: sourceAddress,
            senderUserId: senderUserId,
            identity: identity,
            contact: contact
        )
        FriendRequestSender.sendMutualFriendRequest(
            store: store,
            identity: identity,
            contact: contact,
            displayName: ProfileStore.loadDisplayName()
        )
        ProfileSyncSender.queueToContact(
            store: store,
            identity: identity,
            contact: contact,
            displayName: ProfileStore.loadDisplayName(),
            epoch: ProfileStore.loadOwnAvatarEpoch()
        )
        FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
        ChatEvents.notifyChatChanged(senderUserId)
        if !wasKnown {
            FriendImportEvents.notify(FriendImportEvent(
                contact: contact,
                directBluetooth: sourceAddress != nil
            ))
            incomingAnnouncements.announceFriendAdded(contact: contact)
        }
    }

    /// Hidden-kind rows (endpoint hints, directories, introductions, group
    /// invites) are never on screen in the 1:1 chat, so they ack DELIVERED
    /// only -- never READ. See `PeerStreamWatermark` for `atLeastLamport`.
    private func acknowledgeHiddenMessage(
        sourceAddress: String?,
        senderUserId: Data,
        identity: Identity,
        contact: Contact,
        atLeastLamport: UInt64 = 0
    ) {
        let through = PeerStreamWatermark.through(
            store: store,
            chatId: senderUserId,
            senderUserId: senderUserId,
            atLeastLamport: atLeastLamport
        )
        try? store.recordOutgoingReceipt(
            chatId: senderUserId,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: through
        )
        if queueOutgoingReceiptForRelay(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.delivered,
            ackedSenderUserId: senderUserId,
            throughLamport: through
        ) {
            RelaySyncEvents.requestSync()
        }
        if let sourceAddress {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: sourceAddress,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        } else {
            sendReceiptToContact(
                identity: identity,
                contact: contact,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        }
    }

    private func deliverCarriedMessagesForImportedGroup(group: Group, identity: Identity) {
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let hints = recentHintsFor(userId: group.id, nowMs: now)
        let carried = (try? store.carriedEnvelopesForHints(hints: hints, nowMs: now)) ?? []
        for envelope in carried {
            guard let opened = try? openGroupMessage(group: group, sealed: envelope.sealed) else { continue }
            do {
                try deliverOpenedGroupEnvelope(
                    sourceLabel: "carry queue",
                    group: group,
                    opened: opened,
                    identity: identity,
                    msgId: envelope.msgId,
                    arrival: nil
                )
            } catch {
                // T4-06: best-effort drain -- a store failure must not abort
                // the loop. The carried envelope is left in place (this path
                // never removes it), so a later import/trigger retries it.
                log.warning("Deferring carried group message: durable delivery failed")
            }
        }
    }

    /// Returns the sealed bytes queued.
    ///
    /// This is the encounter's largest lane by some distance -- every group
    /// envelope this device ever authored, for every shared group -- and the
    /// one lane no spray plan can see, so the caller charges it against the
    /// link's burst allowance. It is deliberately not truncated: the walk
    /// always restarts at lamport 0, so cutting it would make later envelopes
    /// unreachable. Charging it means the *next* trigger on this link waits for
    /// the radio instead (#280).
    private func groupDigestAnswerKey(peerUserId: Data, groupId: Data) -> String {
        "\(UserIdHex.encode(peerUserId))/\(UserIdHex.encode(groupId))"
    }

    @discardableResult
    private func resendGroupOutboundToPeer(
        address: String,
        peerUserId: Data,
        identity: Identity,
        afterLamport: UInt64,
        onlyGroupId: Data? = nil,
        skipAnsweredGroups: Bool = false
    ) -> Int {
        var queuedBytes = 0
        let groups = (try? store.listGroups()) ?? []
        for group in groups where group.memberUserIds.contains(peerUserId)
            && group.memberUserIds.contains(identity.userId) {
            if let onlyGroupId, group.id != onlyGroupId { continue }
            if skipAnsweredGroups,
               groupDigestAnswers.contains(groupDigestAnswerKey(peerUserId: peerUserId, groupId: group.id)) {
                continue
            }
            let envelopes = (try? store.outboundEnvelopesAfter(
                chatId: group.id,
                senderUserId: identity.userId,
                afterLamport: afterLamport
            )) ?? []
            for envelope in envelopes {
                if envelope.kind == ProtocolKind.groupInvite,
                   envelope.recipientUserId != peerUserId {
                    continue
                }
                if MeshRouter.sendToAddress(
                    address: address,
                    frame: encodeOutboundEnvelopeFrame(envelope)
                ) {
                    queuedBytes += envelope.sealed.count
                }
            }
        }
        return queuedBytes
    }

    // MARK: - Receipts / carry / relay

    /// DESIGN.md §7.3: receipts go first on peer sync because they're the
    /// smallest frames and unblock the most UI. The store persists the latest
    /// cumulative delivered/read watermarks we owe `contact`, so a receipt that
    /// couldn't be sent when it was first observed heals on this reconnect.
    ///
    /// The peer's digest is deliberately not consulted -- see
    /// `ReceiptRepair.owedTo` for why capping these watermarks against it
    /// self-locked the pairing.
    /// Returns the sealed bytes queued, for the link's burst allowance (#280).
    @discardableResult
    private func syncReceiptsFirst(
        identity: Identity,
        contact: Contact,
        address: String
    ) -> Int {
        var queuedBytes = 0
        for owed in ReceiptRepair.owedTo(store: store, peerUserId: contact.userId) {
            queuedBytes += sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: address,
                receiptType: owed.receiptType,
                ackedSenderUserId: contact.userId,
                throughLamport: owed.throughLamport
            )
        }
        return queuedBytes
    }

    /// Returns the sealed bytes queued at `address`, or 0 if nothing went.
    @discardableResult
    private func sendReceiptOnAddress(
        identity: Identity,
        contact: Contact,
        address: String,
        receiptType: UInt8,
        ackedSenderUserId: Data,
        throughLamport: UInt64
    ) -> Int {
        guard let authored = try? store.ensureAuthoredReceipt(
            identity: identity,
            contact: contact,
            ackedSenderUserId: ackedSenderUserId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ) else { return 0 }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        guard MeshRouter.sendToAddress(address: address, frame: authored.frame) else { return 0 }
        return authored.envelope.sealed.count
    }

    private func sendReceiptToContact(
        identity: Identity,
        contact: Contact,
        receiptType: UInt8,
        ackedSenderUserId: Data,
        throughLamport: UInt64
    ) {
        guard let authored = try? store.ensureAuthoredReceipt(
            identity: identity,
            contact: contact,
            ackedSenderUserId: ackedSenderUserId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ) else { return }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        _ = MeshRouter.sendToUserId(userId: contact.userId, frame: authored.frame)
    }

    private func handleIncomingGroupReceipt(
        sourceAddress _: String?,
        envelopeSender: Data,
        receipt: ReceiptContent,
        groupId: Data,
        identity: Identity,
        arrival: MessageArrival
    ) throws {
        guard receipt.senderUserId == identity.userId else { return }
        guard let group = try? store.getGroup(groupId: groupId),
              group.memberUserIds.contains(envelopeSender),
              group.memberUserIds.contains(identity.userId),
              (try? store.getContact(userId: envelopeSender)) != nil else { return }
        SprayPolicy.noteReceiptProgress(peerUserId: envelopeSender)
        try store.recordGroupReceipt(
            groupId: groupId,
            authorUserId: identity.userId,
            memberUserId: envelopeSender,
            receiptType: receipt.receiptType,
            throughLamport: receipt.lamport,
            viaTransport: arrival.transport
        )
        if receipt.receiptType == ReceiptType.delivered {
            try? store.recordDeliveredMetric(
                chatId: groupId,
                throughLamport: receipt.lamport,
                deliveredAtMs: arrival.receivedAt,
                viaTransport: arrival.transport
            )
        }
        ChatEvents.notifyChatChanged(groupId)
    }

    private func emitGroupReceiptsToAuthor(group: Group, authorUserId: Data, identity: Identity) {
        guard let contact = try? store.getContact(userId: authorUserId) else { return }
        var queued = false
        for owed in ReceiptRepair.owedForGroup(store: store, groupId: group.id, authorUserId: authorUserId) {
            if queueOutgoingGroupReceiptForRelay(
                identity: identity,
                author: contact,
                groupId: group.id,
                receiptType: owed.receiptType,
                throughLamport: owed.throughLamport
            ) {
                queued = true
            }
            sendGroupReceiptToContact(
                identity: identity,
                author: contact,
                groupId: group.id,
                receiptType: owed.receiptType,
                throughLamport: owed.throughLamport
            )
        }
        if queued { RelaySyncEvents.requestSync() }
    }

    @discardableResult
    private func syncGroupReceiptsToPeer(
        identity: Identity,
        contact: Contact,
        address: String
    ) -> Int {
        var queuedBytes = 0
        let groups = (try? store.listGroups()) ?? []
        for group in groups where group.memberUserIds.contains(contact.userId)
            && group.memberUserIds.contains(identity.userId) {
            for owed in ReceiptRepair.owedForGroup(store: store, groupId: group.id, authorUserId: contact.userId) {
                queuedBytes += sendGroupReceiptOnAddress(
                    identity: identity,
                    author: contact,
                    groupId: group.id,
                    address: address,
                    receiptType: owed.receiptType,
                    throughLamport: owed.throughLamport
                )
            }
        }
        return queuedBytes
    }

    @discardableResult
    private func queueOutgoingGroupReceiptForRelay(
        identity: Identity,
        author: Contact,
        groupId: Data,
        receiptType: UInt8,
        throughLamport: UInt64
    ) -> Bool {
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let existing = try? store.outgoingReceiptEnvelope(
            chatId: groupId,
            senderUserId: author.userId,
            receiptType: receiptType
        )
        guard let authored = try? store.ensureAuthoredGroupReceipt(
            identity: identity,
            author: author,
            groupId: groupId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: timestamp
        ) else { return false }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        return existing == nil || (existing?.throughLamport ?? 0) < authored.envelope.throughLamport
    }

    @discardableResult
    private func sendGroupReceiptOnAddress(
        identity: Identity,
        author: Contact,
        groupId: Data,
        address: String,
        receiptType: UInt8,
        throughLamport: UInt64
    ) -> Int {
        guard let authored = try? store.ensureAuthoredGroupReceipt(
            identity: identity,
            author: author,
            groupId: groupId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ) else { return 0 }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        guard MeshRouter.sendToAddress(address: address, frame: authored.frame) else { return 0 }
        return authored.envelope.sealed.count
    }

    private func sendGroupReceiptToContact(
        identity: Identity,
        author: Contact,
        groupId: Data,
        receiptType: UInt8,
        throughLamport: UInt64
    ) {
        guard let authored = try? store.ensureAuthoredGroupReceipt(
            identity: identity,
            author: author,
            groupId: groupId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ) else { return }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        _ = MeshRouter.sendToUserId(userId: author.userId, frame: authored.frame)
    }

    @discardableResult
    private func queueOutgoingReceiptForRelay(
        identity: Identity,
        contact: Contact,
        receiptType: UInt8,
        ackedSenderUserId: Data,
        throughLamport: UInt64
    ) -> Bool {
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let existing = try? store.outgoingReceiptEnvelope(
            chatId: contact.userId,
            senderUserId: ackedSenderUserId,
            receiptType: receiptType
        )
        guard let authored = try? store.ensureAuthoredReceipt(
            identity: identity,
            contact: contact,
            ackedSenderUserId: ackedSenderUserId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: timestamp
        ) else { return false }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        return existing == nil || existing!.throughLamport < authored.envelope.throughLamport
    }

    /// Android `backfillOutboundAuthoredEnvelope` twin. Re-seals one locally
    /// authored message the outbound queue no longer holds a sealed copy of,
    /// so this peer's digest can be answered for that lamport.
    ///
    /// Core decides what happens to the rebuilt envelope beyond being returned
    /// here (`outbound_retirement.rs`, #283). A row can be missing because it
    /// predates the outbound-envelope table, because a delivered receipt
    /// retired it, or because a newer generation of a snapshot kind superseded
    /// it; only the first belongs back in the queue, and only core knows which
    /// case this is. This function never assumes: it asks, sends what comes
    /// back, and leaves the queue to the store. The returned `msgId` is the
    /// message's own persisted id, so `GossipState.seenIds` and the
    /// `alreadyOffered` bound in `handleDigest` both keep recognising a
    /// retransmission as one.
    private func backfillOutbound(identity: Identity, contact: Contact, message: StoredMessage) -> OutboundEnvelope? {
        guard let authored = try? store.backfillPairwiseEnvelope(
            identity: identity,
            contact: contact,
            message: message,
            replyToMsgId: nil
        ) else { return nil }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        return authored.envelope
    }

    /// Android `carryForeignEnvelope` twin. Returns `true` if the store
    /// operation completed (whether it newly queued the envelope or found it
    /// already carried) and `false` if the store call itself failed (`try?`
    /// turns a thrown error into `nil`). DTN D4: `processInboundEnvelope`
    /// uses this return value to decide whether it's safe to mark the
    /// envelope's `msgId` seen -- see its doc comment.
    ///
    /// Also the only carry-ingest path on iOS today: relay proxy-fetched
    /// envelopes (FI2, `sourceAddress == nil`) reach `processInboundEnvelope`
    /// and fall into this same function -- there is no iOS twin of Android's
    /// separate `carryRelayEnvelope`/`enqueueRelayCarriedEnvelope` yet, so
    /// `carriedHopTtl` below covers both cases in one place.
    ///
    /// The stored `hopTtl` is `carriedHopTtl` of the received value, not the
    /// value verbatim -- see its doc comment for the full rationale and the
    /// zero-TTL saturation guarantee.
    private func carryForeign(
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data,
        forceFamily: Bool = false
    ) -> Bool {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let isFamily = forceFamily || ((try? store.hintMatchesKnownTarget(hint: recipientHint, nowMs: now)) ?? false)
        guard let stored = try? store.enqueueCarriedEnvelope(
            envelope: CarriedEnvelope(
                msgId: msgId,
                hopTtl: carriedHopTtl(hopTtl),
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed
            ),
            isFamily: isFamily,
            receivedAtMs: now,
            foreignBudgetBytes: MeshDefaults.foreignCarryBudgetBytes
        ) else {
            return false
        }
        if stored, isFamily {
            RelaySyncEvents.requestSync()
        }
        return true
    }

    /// Floods a foreign (not-for-us) envelope onward, Android
    /// `relayForeignEnvelope` twin. The arriving link is excluded from the
    /// fanout below to avoid the trivial echo.
    ///
    /// DTN D4 loop-hazard note: since `processInboundEnvelope` moved to
    /// check-then-record, `GossipState.seenIds` is *not yet* updated for
    /// this `msgId` at the moment this call happens (it's recorded after
    /// this function returns, once the whole terminal branch succeeds -- see
    /// `processInboundEnvelope`'s doc comment). That's still safe against
    /// self-re-ingestion: the arriving link is excluded from the fanout (so
    /// this node can't hand the relayed frame straight back to itself), and
    /// `processInboundEnvelope` runs synchronously per received frame, so
    /// there is no way for this same `msgId` to re-enter
    /// `processInboundEnvelope` on *this* node before the terminal `record`
    /// call a few lines below this call site completes. A frame this node
    /// relays could only loop back from a third node's rebroadcast, which
    /// takes at least one more hop and one more link round-trip -- by then
    /// this node's record has long since happened.
    private func relayForeign(
        sourceAddress: String?,
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data
    ) {
        let remaining = Int(hopTtl)
        guard remaining > 1 else { return }
        let frame = encodeEnvelopeFrame(
            msgId: msgId,
            hopTtl: UInt8(remaining - 1),
            expiry: expiry,
            recipientHint: recipientHint,
            sealed: sealed
        )
        if let sourceAddress {
            _ = MeshRouter.relayToAllExcept(sourceAddress, frame: frame)
        } else {
            _ = MeshRouter.relayToAll(frame: frame)
        }
    }

    /// Hands over every carried envelope destined for the peer that just
    /// HELLO'd on `address` (DESIGN.md §5.3): compute the peer's recent-day
    /// `recipient_hint`s (`deliveryHintsForPeer`) and pull matching envelopes
    /// from the store, and send each on this link. Expired entries are
    /// pruned first.
    ///
    /// `env.hopTtl` here is forwarded verbatim -- it's already
    /// `carriedHopTtl` of what this device originally received, decremented
    /// once at `carryForeign` enqueue time, not the raw value the frame
    /// arrived with. No further decrement happens here.
    ///
    /// DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): this function only
    /// ever *attempts* delivery -- it no longer calls
    /// `store.removeCarriedEnvelope` on a successful
    /// `MeshRouter.sendToAddress`. That return only means a transport
    /// function accepted the write, not that the bytes made it to the peer;
    /// a disconnect mid-transfer used to silently drop the whole write
    /// queue after we'd already deleted our only copy. The carried row is
    /// now removed later, once the peer's own next digest exchange proves
    /// they actually have it -- see `store.coreConfirmCarriedDeliveries`,
    /// called from `sprayDigestPlanTo`.
    ///
    /// Invariant, stated verbatim (DTN_TODOS.md §3.2): worst case of a
    /// dropped mid-transfer link is a harmless duplicate resend (the peer's
    /// seen-set/store dedupes it), never a lost envelope; an unconfirmed
    /// carry still dies at its normal expiry via `store.pruneExpiredCarried`.
    /// Returns the sealed bytes queued at the link, so the caller can charge
    /// them against its burst allowance: this drain is one of the encounter's
    /// largest lanes and no spray plan accounts for it (#280).
    @discardableResult
    private func drainCarriedEnvelopesTo(
        address: String,
        peerUserId: Data,
        carriedBudgetBytes: UInt64
    ) -> Int {
        guard carriedBudgetBytes > 0 else { return 0 }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        try? store.pruneExpiredCarried(nowMs: now)
        // G2: budgeted page + resume cursor (DTN: offer only, never remove on send).
        let lane = MeshRouter.targetedCarriedLaneFor(address: address, nowMs: now)
        if lane.skip { return 0 }
        // G3: HELLO drains share the foreign-carry allowance with digest sprays
        // and reserve by authenticated user, so duplicate roles or rotating
        // addresses for one phone cannot each enqueue a full page inside one
        // connection burst. This lane was the one still running ungated here.
        guard let reservation = CarriedOfferEpochGate.tryReserve(
            nowMs: now,
            logicalPeerId: UserIdHex.encode(peerUserId)
        ) else {
            log.debug("Targeted carried drain deferred for \(address, privacy: .public) (foreign-carry allowance spent this window)")
            return 0
        }
        let hints = (try? store.deliveryHintsForPeer(peerUserId: peerUserId, nowMs: now)) ?? []
        guard let page = try? store.carriedEnvelopesForHintsPage(
            hints: hints,
            nowMs: now,
            budgetBytes: carriedBudgetBytes,
            maxRows: coreCarriedPageMaxRows(),
            after: lane.after
        ) else {
            CarriedOfferEpochGate.release(reservation)
            return 0
        }
        guard !page.rows.isEmpty else {
            // Nothing offered, so the slot returns to the pool for another peer
            // in this window. The lane still records where its walk got to.
            CarriedOfferEpochGate.release(reservation)
            MeshRouter.recordTargetedCarriedProgress(
                address: address,
                next: page.next,
                exhausted: page.exhausted,
                nowMs: now
            )
            return 0
        }
        CarriedOfferEpochGate.commit(reservation)
        var delivered = 0
        var queuedBytes = 0
        for env in page.rows {
            let frame = encodeEnvelopeFrame(
                msgId: env.msgId,
                hopTtl: env.hopTtl,
                expiry: env.expiry,
                recipientHint: env.recipientHint,
                sealed: env.sealed
            )
            if MeshRouter.sendToAddress(address: address, frame: frame) {
                delivered += 1
                queuedBytes += env.sealed.count
            }
        }
        MeshRouter.recordTargetedCarriedProgress(
            address: address,
            next: page.next,
            exhausted: page.exhausted,
            nowMs: now
        )
        if delivered > 0 {
            log.info("Attempted delivery of \(delivered)/\(page.rows.count) carried envelope(s) to \(address, privacy: .public) (budgeted HELLO drain; removal awaits their digest confirmation)")
        }
        return queuedBytes
    }

    /// Executes Rust's complete digest-time mule plan, inside the budgets
    /// `gate` was issued with.
    ///
    /// `gate` is core's answer to "may this peer be sprayed, and how much"
    /// (`SprayPolicy`); the caller has already checked `gate.allow`. Two
    /// further core decisions happen here, both after the plan is built
    /// because both need to know what the plan came out as: whether the
    /// advertised set is byte-identical to the one this peer was already
    /// offered, and what the plan costs this link's burst allowance. A
    /// suppressed plan sends nothing and, just as importantly, advances no
    /// cursor and records no hidden-kind offer — everything it would have
    /// offered stays exactly as re-discoverable as it was.
    private func sprayDigestPlanTo(
        address: String,
        peerUserId: Data,
        peerKnownIds: [Data],
        identity: Identity,
        gate: CoreSprayGate,
        peerAuthenticated: Bool
    ) {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): confirm delivery of
        // anything this digest's advertised msg_ids prove the peer already
        // has BEFORE building the spray plan below, so a just-confirmed
        // carried envelope isn't immediately re-sprayed back at the peer who
        // just told us they have it.
        //
        // CARRY-02: durable removal of a carried row is only permitted when the
        // peer identity is authenticated. `peerAuthenticated` is passed in, not
        // re-derived from `address`, because it must reflect the transport the
        // digest ARRIVED on -- a `.lan` transport filed only after a completed
        // Noise handshake whose static key matched an accepted contact
        // (`lan.onAuthenticated`) -- and NOT the link this response is answered
        // on. On the gated-then-replayed path the elected route may have moved
        // to LAN since a Bluetooth digest arrived; re-deriving here would
        // launder that digest's unsigned, spoofable userId and advertised
        // msg_ids into an authenticated removal. For an unauthenticated peer
        // this call deletes nothing and only lets the spray plan skip the ids
        // the peer named for this one encounter.
        if let confirmed = try? store.coreConfirmCarriedDeliveries(
            peerUserId: peerUserId,
            peerKnownMsgIds: peerKnownIds,
            peerAuthenticated: peerAuthenticated,
            nowMs: now
        ), confirmed > 0 {
            log.info("Confirmed delivery of \(confirmed) carried envelope(s) to \(UserIdHex.encode(peerUserId), privacy: .public); dropped our copy")
            // Hard evidence that sprays to this peer are landing: it just
            // proved it holds copies we were carrying. That is what the
            // receipt-quiet backoff is looking for. Its absence is NOT
            // evidence of a fault — a courier for someone who is not here
            // legitimately confirms nothing.
            SprayPolicy.noteReceiptProgress(peerUserId: peerUserId)
        }
        // How far this link session's walk through our carry queue has got. A
        // courier's store can be many times one round's budget, so each
        // re-digest offers the NEXT page instead of re-reading the oldest
        // rows; once the walk reaches the tail the lane parks until its
        // cooldown elapses. A zero budget is the lane's own off switch.
        let lane = MeshRouter.carriedLaneFor(address: address, nowMs: now)
        // G3: cap concurrent foreign-carry offers across peers in a short
        // window, reserving by authenticated user rather than by link address
        // so one phone reaching us twice cannot claim the whole allowance. Own
        // mail and receipts still flow when carried is deferred.
        let reservation = lane.skip
            ? nil
            : CarriedOfferEpochGate.tryReserve(
                nowMs: now,
                logicalPeerId: UserIdHex.encode(peerUserId)
            )
        let allowCarried = reservation != nil
        guard let plan = try? store.coreDigestSprayPlan(
            ownUserId: identity.userId,
            peerUserId: peerUserId,
            peerHints: recentHintsFor(userId: peerUserId, nowMs: now),
            peerKnownMsgIds: peerKnownIds,
            nowMs: now,
            carriedBudgetBytes: allowCarried ? gate.carriedBudgetBytes : 0,
            ownOutboundBudgetBytes: gate.ownOutboundBudgetBytes,
            ownReceiptBudgetBytes: gate.ownReceiptBudgetBytes,
            receiptQueryLimit: MeshDefaults.relayStoreBatchLimit,
            peerAcksHiddenKinds: MeshRouter.peerAckedHiddenKinds(address: address),
            hiddenAlreadyOffered: MeshRouter.hiddenOfferedFor(address: address),
            carriedCursor: lane.after
        ) else {
            if let reservation { CarriedOfferEpochGate.release(reservation) }
            log.warning("Failed to build digest spray plan for \(address, privacy: .public)")
            return
        }
        // Identical-set suppression (#280), asked per lane: 28 consecutive
        // sprays whose authored lane was invariant at 16 envelopes while the
        // carried lane walked its cursor is what the field recorded, and one
        // digest over all three would change on every page turn. Asked here
        // rather than before the plan because the answer is the plan.
        let admission = SprayPolicy.admitPlan(
            peerUserId: peerUserId,
            address: address,
            lanes: plan.lanes
        )
        let sendCarried = allowCarried && admission.sendCarried
        if let reservation {
            // A plan that offers nothing gives its slot back, so a peer that
            // was going to be sprayed in this window still can be.
            if !sendCarried || plan.carriedFrames.isEmpty {
                CarriedOfferEpochGate.release(reservation)
            } else {
                CarriedOfferEpochGate.commit(reservation)
            }
        }
        guard admission.send else {
            log.info("Suppressed an unchanged digest spray to \(address, privacy: .public) (\(plan.planBytes) bytes, re-offerable in \(admission.reofferInMs)ms)")
            return
        }
        // Own lanes first, foreign carry last. On a slow link every frame here
        // lands in one FIFO, so whatever goes first delays everything after
        // it: live mail and receipts to real contacts must beat third-party
        // courier traffic. Nothing is lost by deferring the carried lane --
        // the periodic re-digest offers the next page, and its own
        // per-encounter budget already bounds this round's share.
        var frames: [Data] = []
        if admission.sendOwnOutbound { frames += plan.ownOutboundFrames }
        if admission.sendOwnReceipts { frames += plan.ownReceiptFrames }
        if sendCarried { frames += plan.carriedFrames }
        for frame in frames {
            _ = MeshRouter.sendToAddress(address: address, frame: frame)
        }
        // A refused lane must leave its bookkeeping alone: nothing it would
        // have offered may look offered.
        if admission.sendOwnOutbound {
            MeshRouter.recordHiddenOffered(address: address, msgIds: plan.offeredHiddenMsgIds)
        }
        if sendCarried {
            MeshRouter.recordCarriedProgress(
                address: address,
                next: plan.nextCarriedCursor,
                exhausted: plan.carriedExhausted,
                nowMs: now
            )
        }
    }

    // Hint aggregation (recent/delivery/known-target/group matching) moved
    // into the Rust core (FA15 follow-up) -- see core/src/recipient_hints.rs.
    // MeshService.kt made the same move, so both shells share one window and
    // one implementation.

    // MARK: - Relay

    private func startRelayLoop() {
        // Push health is unknown at this point (relayPushClient hasn't been
        // (re)started for this session yet) -- start at the
        // unhealthy/background cadence; the first real health report
        // reschedules from there via onRelayPushHealthChanged.
        lastKnownPushHealthy = nil
        relayTimer?.cancel()
        relayTimer = meshTimer(
            intervalSeconds: TimeInterval(RelayPollPolicy.unhealthyOrBackgroundMs) / 1_000,
            repeats: false
        ) { [weak self] in
            self?.relayPollTick()
        }
        pathMonitor = NWPathMonitor()
        pathMonitor?.pathUpdateHandler = { [weak self] path in
            // Keep the diagnostics banner's view of the network current
            // without standing up a second NWPathMonitor just to print it.
            EnvironmentSnapshot.record(path: path)
            self?.meshQueue.async {
                guard let self else { return }
                if path.status == .satisfied {
                    self.runRelaySync()
                }
                // Recheck the push subscription on every path change, mirroring
                // Android's relayNetworkCallback calling updateRelayPushSubscription
                // from both onCapabilitiesChanged and onLost -- the push socket
                // should be up in exactly the situations runRelaySync would
                // already succeed in, and torn down the moment that stops being
                // true.
                self.updateRelayPushSubscription()
            }
        }
        pathMonitor?.start(queue: .global(qos: .utility))

        // Immediate kick on send
        relayCancellable = RelaySyncEvents.subject.sink { [weak self] in
            self?.meshQueue.async { self?.runRelaySync() }
        }
        updateRelayPushSubscription()
    }

    /// Network facts only. iOS has no public roaming bit — `CTCarrier` was
    /// deprecated in iOS 16 and reports dummy values on current releases — so
    /// core receives `.unknown` and deliberately does not infer roaming from
    /// `isExpensive`, which only means cellular and would switch Shore Pass
    /// off for every iPhone that is away from Wi-Fi at home. A roaming iPhone
    /// is already protected by the system Data Roaming setting, which blocks
    /// the traffic at the modem. `isConstrained` (Low Data Mode) is a real
    /// user signal and still defers the carried lane.
    private func relayNetworkVerdict(_ path: NWPath?) -> CoreRelayNetworkVerdict {
        guard let path else { return .permitted }
        return coreRelayNetworkPermitted(
            roaming: .unknown,
            constrained: path.isConstrained,
            userAllowsRoaming: RelayEngineSettings.allowsRoamingData()
        )
    }

    /// `relayTimer`'s tick: runs the authoritative poll, then reschedules
    /// itself at whatever interval `RelayPollPolicy` currently calls for.
    /// Battery, 2026-07-21: replaces the old fixed-60s repeating timer with a
    /// self-rescheduling one-shot timer (mirrors Android's
    /// `relayPollRunnable` `Runnable.postDelayed` self-repost) so the cadence
    /// can change on every tick. The poll call itself (`runRelaySync`) is
    /// unchanged and stays correctness-authoritative; only its cadence
    /// changes.
    private func relayPollTick() {
        runRelaySync()
        reschedulePoll(currentlyHealthy: relayPushClient.isHealthy())
    }

    /// Recomputes the relay-poll interval from
    /// `RelayPollPolicy.relayPollIntervalMs` given `currentlyHealthy` and the
    /// current `appForeground` state, and re-arms `relayTimer` with it,
    /// cancelling whatever was previously scheduled. Called from
    /// `relayPollTick` itself (every tick decides its own next interval),
    /// from `onRelayPushHealthChanged` (so a health transition reschedules
    /// immediately rather than waiting out whatever long interval is already
    /// pending), and from `setAppForeground` (so a foreground/background flip
    /// reschedules immediately too). Mirrors Android's
    /// `MeshService.reschedulePoll`.
    private func reschedulePoll(currentlyHealthy: Bool) {
        let interval = RelayPollPolicy.relayPollIntervalMs(
            previouslyHealthy: lastKnownPushHealthy,
            currentlyHealthy: currentlyHealthy,
            foreground: appForeground
        )
        lastKnownPushHealthy = currentlyHealthy
        relayTimer?.cancel()
        relayTimer = meshTimer(
            intervalSeconds: TimeInterval(interval) / 1_000,
            repeats: false
        ) { [weak self] in
            self?.relayPollTick()
        }
    }

    /// `relayPushClient`'s health-change callback -- see `relayPushClient`'s
    /// doc and `RelayPollPolicy`'s type doc. Also mirrors the signal into
    /// `MeshConnectivityStatus.pushHealthy` for `level(for:)`'s relay-health
    /// freshness check: without this, "Online via relay" would falsely
    /// degrade after ~120-150s of push-healthy-but-quiet, since the poll
    /// (which used to be the only thing refreshing `RelayHealth.ok`'s
    /// `lastSyncMs`) now backs off to 900s while foregrounded with push up.
    private func onRelayPushHealthChanged(_ healthy: Bool) {
        log.info("Relay push health -> \(healthy)")
        reschedulePoll(currentlyHealthy: healthy)
        onMain { MeshConnectivityStatus.shared.setPushHealthy(healthy) }
    }

    /// Starts `relayPushClient` against the user's relay config once the mesh
    /// is running, an identity and relay config exist, and the current
    /// network path is satisfied -- or stops it otherwise. Called from
    /// `startRelayLoop` and on every path update, mirroring the points
    /// `runRelaySync` is itself kicked from (Android
    /// `updateRelayPushSubscription` parity, DTN_TODOS.md D3).
    ///
    /// The hint set passed to `RelayPushClient.start` is recomputed on every
    /// (re)connect via `relayPushHints`, so a contact or group added after
    /// the socket is already open is picked up the next reconnect without
    /// this needing its own change tracking; until then the 60s poll already
    /// covers it.
    private func updateRelayPushSubscription() {
        guard isRunning,
              let identity,
              let config = RelayConfigStore.load(),
              pathMonitor?.currentPath.status == .satisfied,
              relayNetworkVerdict(pathMonitor?.currentPath) != .deferredRoaming
        else {
            relayPushClient.stop()
            return
        }
        let ownUserId = identity.userId
        let subscribeConfig = config
        RemotePushRegistrationClient.sync(config: config, ownUserId: ownUserId)
        relayPushClient.start(config: config) {
            relayPushSubscription(ownUserId: ownUserId, config: subscribeConfig)
        }
    }

    @discardableResult
    private func runRelaySync() -> Bool {
        guard isRunning, let identity else { return false }
        // No own config is no longer a hard stop: contacts' friend cards can
        // carry relays worth polling (Android parity). relaySyncBlocking
        // reports .noConfig when there is truly nowhere to sync.
        let config = RelayConfigStore.load()
        if let config {
            RemotePushRegistrationClient.sync(config: config, ownUserId: identity.userId)
        }
        guard pathMonitor?.currentPath.status == .satisfied else {
            onMain { MeshConnectivityStatus.shared.setRelayHealth(.noInternet) }
            return false
        }
        if relayNetworkVerdict(pathMonitor?.currentPath) == .deferredRoaming {
            // A policy deferral is offline-like: it starts no pass and cannot
            // affect failure streaks, endpoint rests, or family backoff.
            onMain { MeshConnectivityStatus.shared.setRelayHealth(.deferredRoaming) }
            updateRelayPushSubscription()
            return false
        }
        // CP2b: honor relayd's Retry-After. Nudges inside the advertised
        // window (poll tick, push frame, queue change) are dropped; the 60 s
        // poll tick retries once the window has passed. Mirrors
        // RelaySyncEngine.kt's coalesced backoff.
        if Int64(Date().timeIntervalSince1970 * 1_000) < relayRateLimitedUntilMs {
            return false
        }
        if relaySyncInFlight {
            relaySyncPending = true
            return true
        }
        relaySyncInFlight = true
        backfillRelayReceipts(identity: identity)
        // T23: if our own endpoint changed since the last announcement, queue
        // the notice to every contact *before* this pass uploads, so it rides
        // out in the same sync. This is the single trigger for every way the
        // config can change (Shore Pass setup and removal, manual entry in
        // Advanced, a scanned setup card, a backup restore) because they all
        // already end in `RelaySyncEvents.requestSync()` — no save site has to
        // remember to announce, and none can be missed. Mirrors Android's
        // `RelaySyncEngine.performRelaySyncPass`.
        RelayUpdateSender.announceIfChanged(store: store, identity: identity)
        // DL-3 / §9.5 / §10.1's contact leg, on the same catch-up footing and for
        // the same reason: this person's own device list is a fact every contact
        // has to learn, and core's per-contact ledger makes asking on every pass
        // cost one query on the install that has nothing to say -- which is
        // nearly every install. The link and remove journeys fire it at the
        // moment they change something; this is the repair pass that catches a
        // contact added since, or a copy that expired unread. Mirrors Android's
        // `RelaySyncEngine`.
        RosterGossipSender.announceIfOwed(store: store, identity: identity)
        onMain { MeshRuntimeStatus.shared.markSyncingViaRelay() }
        Task.detached(priority: .utility) { [weak self] in
            guard let self else { return }
            await self.relaySyncBlocking(identity: identity, config: config)
            self.meshQueue.async { self.finishRelaySync() }
        }
        return true
    }

    /// `RATE-01`'s second clause: a nudge that arrived while the pass was in
    /// flight may not start a fresh pass inside the quiet window that pass
    /// recorded.
    ///
    /// The decision is the core's (`core_relay_rerun_action`), and asking it
    /// here is a change of kind rather than of behaviour. It used to be
    /// implicit: the pending nudge re-entered `runRelaySync`, whose front-door
    /// gate dropped it, and what actually re-ran the pass was the retry work
    /// item armed when the 429 was recorded. Two gates agreeing is not the same
    /// as a decision, and Android had already made it one
    /// (`relayRerunAction`). Now both shells ask the same function at the same
    /// point, and the deferral re-arms the coalesced retry rather than relying
    /// on a timer set elsewhere still being alive.
    private func finishRelaySync() {
        relaySyncInFlight = false
        finishAllRemoteRelayWakes(completed: true)
        let pending = relaySyncPending
        relaySyncPending = false
        let backoffRemainingMs = relayRateLimitedUntilMs
            - Int64(Date().timeIntervalSince1970 * 1_000)
        switch coreRelayRerunAction(
            pendingRequested: pending,
            canSync: isRunning,
            backoffRemainingMs: backoffRemainingMs
        ) {
        case .runAgain:
            runRelaySync()
        case .scheduleRateLimitRetry:
            scheduleRelayRateLimitRetry(afterMs: backoffRemainingMs)
        case .stop:
            refreshNearby()
        }
    }

    private func backfillRelayReceipts(identity: Identity) {
        // Core refreshes every contact's DELIVERED/READ receipt envelopes for
        // the current watermarks (was an inline loop here and in
        // MeshService.kt); the returned msg_ids seed the in-memory seen-set
        // for the same reason queueOutgoingReceiptForRelay records there --
        // our own receipt envelope coming back off the relay must dedupe,
        // not get re-carried as foreign mail.
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let msgIds = (try? store.backfillOutgoingReceiptEnvelopes(identity: identity, nowMs: now)) ?? []
        for msgId in msgIds {
            GossipState.seenIds.record(msgId: msgId)
        }
    }

    /// Core owns the mailbox-routing policy (T11): an envelope addressed to a
    /// contact goes to THAT contact's relay mailbox (from their friend card),
    /// falling back to our own saved config. relayd scopes rows per family
    /// token, so posting a cross-family friend request to our own mailbox
    /// strands it where the recipient can never fetch it. Mirrors
    /// RelaySyncEngine.kt's resolvedRelayConfig.
    ///
    /// `endpointUsable: false` means their card endpoint has authoritatively
    /// rejected us and has been written off (core `contact_relay_health`), so
    /// resolution skips it exactly as though the card carried no relay
    /// fields — otherwise one dead field beats a working alternative forever.
    /// Mirrors RelaySyncEngine.kt's resolvedRelayConfig.
    static func resolvedRelayConfig(
        contact: Contact?,
        fallback: RelayConfig?,
        endpointUsable: Bool = true
    ) -> RelayConfig? {
        resolvedContactDeliveryRelay(
            contactRelayUrl: contact?.relayUrl,
            contactRelayToken: contact?.relayToken,
            fallbackUrl: fallback?.relayUrl,
            fallbackToken: fallback?.relayToken,
            contactEndpointUsable: endpointUsable
        ).map { RelayConfig(relayUrl: $0.url, relayToken: $0.token) }
    }

    /// CP4: fetch/ack/presence resolution. Post-CP4 friend cards carry
    /// post-only deposit tokens, which cannot read a mailbox — the core
    /// resolves a same-family card back to our own member config and drops
    /// cross-family deposit endpoints entirely (polling them would just 403
    /// `deposit_only` on every pass). Legacy member-token cards keep
    /// proxy-polling exactly as before. Sends stay on `resolvedRelayConfig`.
    /// Mirrors RelaySyncEngine.kt's resolvedPollRelayConfig.
    static func resolvedPollRelayConfig(
        contact: Contact?,
        fallback: RelayConfig?,
        endpointUsable: Bool = true
    ) -> RelayConfig? {
        resolvedContactDeliveryPollRelay(
            contactRelayUrl: contact?.relayUrl,
            contactRelayToken: contact?.relayToken,
            fallbackUrl: fallback?.relayUrl,
            fallbackToken: fallback?.relayToken,
            contactEndpointUsable: endpointUsable
        ).map { RelayConfig(relayUrl: $0.url, relayToken: $0.token) }
    }

    /// Whether a contact's card endpoint has earned another attempt, given
    /// the rejection streaks read once at the start of this pass. Mirrors
    /// RelaySyncEngine.kt's contactEndpointUsable.
    static func contactEndpointUsable(
        contact: Contact,
        rejections: [Data: ContactRelayRejection],
        nowMs: Int64
    ) -> Bool {
        guard let rejection = rejections[contact.userId] else { return true }
        return coreContactRelayEndpointUsable(
            rejectStreak: rejection.rejectStreak,
            rejectedAtMs: rejection.rejectedAtMs,
            nowMs: nowMs
        )
    }

    /// Every distinct mailbox this device should poll: its own saved config
    /// first, then each contact's resolved card relay (mirrors
    /// RelaySyncEngine.kt's distinctRelayConfigs).
    static func distinctRelayConfigs(
        contacts: [Contact],
        fallback: RelayConfig?,
        rejections: [Data: ContactRelayRejection] = [:],
        nowMs: Int64 = 0
    ) -> [RelayConfig] {
        var result: [RelayConfig] = []
        func add(_ cfg: RelayConfig?) {
            guard let cfg else { return }
            if !result.contains(where: { $0.relayUrl == cfg.relayUrl && $0.relayToken == cfg.relayToken }) {
                result.append(cfg)
            }
        }
        add(fallback)
        for contact in contacts {
            if let cfg = Self.resolvedPollRelayConfig(
                contact: contact,
                fallback: fallback,
                endpointUsable: Self.contactEndpointUsable(
                    contact: contact,
                    rejections: rejections,
                    nowMs: nowMs
                )
            ) {
                if !result.contains(where: { $0.relayUrl == cfg.relayUrl && $0.relayToken == cfg.relayToken }) {
                    result.append(cfg)
                    relaySyncLog.info("Secondary relay poll config added for contact \(UserIdHex.encode(contact.userId), privacy: .public): \(cfg.relayUrl, privacy: .public)")
                }
            }
        }
        return result
    }

    /// The relay sync pass. Runs on a detached task, off `meshQueue` and off
    /// the main thread: it is a long sequence of blocking HTTP calls, and it
    /// serialises against itself through `relaySyncInFlight` rather than
    /// through any queue. Deliberately unchanged by the move of the inbound
    /// pipeline off the main thread -- including `ContactRelaySilence`, whose
    /// per-pass bookkeeping is touched here and nowhere else, so it still only
    /// ever sees one pass at a time.
    ///
    /// The one point where it meets the mesh pipeline is per-envelope
    /// delivery, which goes through `onMeshQueue` so a relay-fetched envelope
    /// is processed under the same serial guarantee a BLE frame gets.
    private func relaySyncBlocking(identity: Identity, config capturedConfig: RelayConfig?) async {
        // §10 step 2, before either engine and belonging to neither: a device
        // removal wrote a rotation down and stopped, and this is the first
        // place off the mesh queue that can perform it. Here rather than in the
        // removal journey because a removal must not fail on connectivity, and
        // here rather than in `runRelaySync` because that runs on the mesh queue
        // and this makes a network call. The driver paces itself, so a pass that
        // finds a rotation it may not retry yet costs one query.
        //
        // Re-read when it moves this device's pass: the credential handed to
        // this pass on the mesh queue is the one the relay has just retired,
        // and carrying it on would 401 every upload and fetch below, ending the
        // pass by telling the person their Shore Pass is broken -- on the very
        // pass where their removal worked.
        var config = capturedConfig
        if rotateFamilyTokenIfOwed(identity: identity) {
            config = RelayConfigStore.load()
        }

        // C2: whole-pass engine selection, read once here at pass start and not
        // consulted again — the flip cannot mix engines within a pass. Default
        // legacy; the core path runs only when the internal switch is on. When
        // it is, the legacy pass below (and its canary) never execute, which is
        // the other half of "one engine writes the production store per pass".
        if RelayEngineSettings.passEngine() == .core {
            await performCoreRelaySyncPass(identity: identity, config: config)
            return
        }
        let store = AppStore.get()
        // T11 + CP2b: structured rejections of our OWN saved config -- a
        // contact's stale card relay failing is not our pass's fault. The
        // classification (HTTP status/`code` -> semantic fault, transient vs
        // persistent) lives in the core (`core/src/relay_status.rs`); an
        // unstructured failure (.outage) is not recorded because the pass's
        // success flags already express it as .failing. Mirrors
        // RelaySyncEngine.kt's noteOwnRelayFault.
        var ownRelayFault: CoreRelayFault?
        var familyRetryDelayMs: UInt64 = 0
        func noteFailure(_ error: Error, usedConfig: RelayConfig) {
            guard let own = config,
                  usedConfig.relayUrl == own.relayUrl,
                  usedConfig.relayToken == own.relayToken else { return }
            guard let relay = error as? RelayHTTPError else { return }
            let fault = relayClassifyHttpError(
                httpStatus: UInt16(clamping: relay.statusCode),
                relayCode: relay.relayCode
            )
            guard fault != .outage else { return }
            ownRelayFault = RelayHealth.worseFault(ownRelayFault, fault)
        }
        // The core derives this pass's anti-lockstep offset from the PUBLIC
        // user id (see FamilyRelayBackpressure.swift); no hash is computed on
        // this side any more.
        let identityPublicBytes = identity.userId
        func relayRequest<T>(_ operation: () throws -> T) throws -> T {
            // Monotonic on purpose: the pacer's reservation must not be
            // rewound by a wall-clock correction.
            let monotonicNowMs = Int64(clamping: DispatchTime.now().uptimeNanoseconds / 1_000_000)
            let waitMs = familyRelayRequestPacer.reserve(nowMs: monotonicNowMs)
            if waitMs > 0 {
                Thread.sleep(forTimeInterval: Double(waitMs) / 1_000)
            }
            do {
                return try operation()
            } catch let relay as RelayHTTPError {
                let fault = relayClassifyHttpError(
                    httpStatus: UInt16(clamping: relay.statusCode),
                    relayCode: relay.relayCode
                )
                guard fault == .rateLimited else { throw relay }
                let advertisedMs = relayRetryAfterMs(retryAfterHeader: relay.retryAfter)
                let delayMs = familyRelayBackoff.onRateLimited(
                    retryAfterMs: advertisedMs,
                    identityPublicBytes: identityPublicBytes
                )
                // A 429 is a family-token budget verdict even when this
                // particular request used a contact-resolved config. Surface
                // it and halt the whole pass rather than spending more budget.
                ownRelayFault = RelayHealth.worseFault(ownRelayFault, .rateLimited)
                familyRetryDelayMs = max(familyRetryDelayMs, delayMs)
                throw FamilyRelayRateLimitAbort(retryDelayMs: delayMs)
            }
        }
        func rethrowFamilyRateLimit(_ error: Error) throws {
            if error is FamilyRelayRateLimitAbort { throw error }
        }
        do {
            // Upload receipts first, then authored, then family carry --
            // each to the recipient's resolved mailbox, and each post in its
            // own catch so one dead contact relay can't stall the pass
            // (mirrors RelaySyncEngine.kt).
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            _ = try store.pruneExpiredOutgoingReceiptEnvelopes(nowMs: now)
            _ = try store.pruneExpiredOutboundEnvelopes(nowMs: now)
            _ = try store.pruneExpiredCarried(nowMs: now)
            // Same expiry-driven family: once an envelope is expired its relay
            // copy is ackable on the `.expired` disposition alone, so the
            // record that this device consumed it has nothing left to prove.
            _ = try store.pruneExpiredConsumedHiddenMsgIds(nowMs: now)
            let contacts = try store.listContacts()
            let contactsById = Dictionary(
                uniqueKeysWithValues: contacts.map { ($0.userId, $0) }
            )
            // The other half of noteFailure, which had no owner before this:
            // rejections from the endpoint in a CONTACT's friend card. Those
            // were dropped entirely, so a card pointing at a retired host
            // produced an unbounded silent retry loop while the person's
            // messages sat at one tick. Read both health records once per
            // pass; only contacts with a non-zero streak appear. Mirrors
            // RelaySyncEngine.kt.
            var rejections = Dictionary(
                uniqueKeysWithValues: (try store.listContactRelayRejections()).map { ($0.userId, $0) }
            )
            var unreachable = Dictionary(
                uniqueKeysWithValues: (try store.listContactRelayUnreachable()).map { ($0.userId, $0) }
            )
            func endpointUsable(_ contact: Contact) -> Bool {
                Self.contactEndpointUsable(contact: contact, rejections: rejections, nowMs: now)
            }
            /// Contacts whose streak already advanced during this pass.
            ///
            /// The core's threshold is worded in *passes* ("requiring the next
            /// pass to agree" -- `CONTACT_RELAY_STALE_STREAK`), which is what
            /// rules out a relay answering mid-redeploy from a
            /// half-initialised process. Counting per envelope quietly broke
            /// that: a contact with two queued messages was written off inside
            /// a single pass, the exact false positive the second pass exists
            /// to prevent. Mirrors RelaySyncEngine.kt.
            var countedThisPass: Set<Data> = []
            // Contacts whose endpoint fails this pass without answering are
            // held by ContactRelaySilence until the end of the pass, because
            // the observation only means anything next to proof that this
            // device's internet works -- and, from this pass forward, so the
            // rest of the pass stops dialling an address that just refused.
            ContactRelaySilence.shared.restore(Array(unreachable.values))
            ContactRelaySilence.shared.beginPass()
            /// A rest belongs to an *address*, not to a person: `relayCursorKey`
            /// hashes the contact's current endpoint so a card or a T23 notice
            /// that moves them to a different host is tried again immediately
            /// instead of serving out the old host's rest window.
            func endpointKey(_ contact: Contact) -> String {
                relayCursorKey(
                    relayUrl: contact.relayUrl ?? "",
                    relayToken: contact.relayToken ?? ""
                )
            }
            /// Whether this contact's endpoint has answered recently enough to
            /// be worth a request. The counterpart to `endpointUsable` for the
            /// failure mode with no HTTP answer to classify.
            func endpointAnswering(_ contact: Contact) -> Bool {
                ContactRelaySilence.shared.endpointAnswering(
                    userId: contact.userId,
                    endpointKey: endpointKey(contact),
                    nowMs: now
                )
            }
            /// Where a send to this contact should go, or nil for "no relay
            /// attempt right now", which leaves the envelope queued for a
            /// later pass and for the BLE/LAN paths.
            ///
            /// Nil is deliberately the answer for a *silent* endpoint, and it
            /// is not the answer a rejected one gets. A rejection proves the
            /// card is wrong, so falling back to our own relay costs nothing
            /// and delivers outright when both sides have since moved to the
            /// same new host. Silence proves nothing -- the host may be
            /// rebooting -- and falling back would post a cross-family
            /// contact's mail into our own mailbox, which they never read.
            /// `relayPostedAt` is terminal, so that misroute would not be a
            /// retry: the envelope would never be offered to the relay path
            /// again. Mirrors RelaySyncEngine.kt's resolvedRelayConfig.
            func sendConfig(for contact: Contact) -> RelayConfig? {
                guard endpointAnswering(contact) else { return nil }
                return Self.resolvedRelayConfig(
                    contact: contact,
                    fallback: config,
                    endpointUsable: endpointUsable(contact)
                )
            }
            /// Any HTTP answer settles transport silence, including a non-2xx
            /// response that may advance the separate rejection streak.
            func noteContactAnswered(_ contact: Contact) {
                ContactRelaySilence.shared.noteAnswered(userId: contact.userId)
                try? store.clearContactRelayUnreachable(userId: contact.userId)
                unreachable[contact.userId] = nil
            }
            /// Only counts when `usedConfig` is genuinely the contact's own
            /// endpoint: once we have fallen back to our own relay, a failure
            /// there is our relay's health, not evidence about their card.
            func noteContactFailure(_ error: Error, contact: Contact, usedConfig: RelayConfig) {
                if let own = config,
                   usedConfig.relayUrl == own.relayUrl,
                   usedConfig.relayToken == own.relayToken { return }
                guard let relay = error as? RelayHTTPError else {
                    // No HTTP answer at all -- a retired host, dead DNS, a
                    // refused connection. Not evidence about the card on its
                    // own, so it is only remembered here; the end of the pass
                    // decides whether this device had any business believing
                    // it.
                    //
                    // Logged on the transition only. The per-envelope upload
                    // warnings name the host but not whose card carries it,
                    // which left a field report of hundreds of failures
                    // against one URL with no way to tell which contact to ask
                    // for a fresh card.
                    if ContactRelaySilence.shared.noteUnreachableThisPass(
                        userId: contact.userId,
                        endpointKey: endpointKey(contact)
                    ) {
                        let contactId = relayDiagnosticContactId(contact.userId)
                        let relayHost = relayDiagnosticHost(usedConfig.relayUrl)
                        relaySyncLog.warning(
                            "Contact \(contactId, privacy: .public) relay host=\(relayHost, privacy: .public) did not answer: \(error.localizedDescription, privacy: .public)"
                        )
                    }
                    return
                }
                noteContactAnswered(contact)
                let fault = relayClassifyHttpError(
                    httpStatus: UInt16(clamping: relay.statusCode),
                    relayCode: relay.relayCode
                )
                guard coreContactRelayStreakDelta(fault: fault) != 0 else { return }
                guard countedThisPass.insert(contact.userId).inserted else { return }
                guard let streak = try? store.noteContactRelayRejected(userId: contact.userId, nowMs: now) else {
                    return
                }
                rejections[contact.userId] = ContactRelayRejection(
                    userId: contact.userId,
                    rejectStreak: streak,
                    rejectedAtMs: now
                )
                let contactId = relayDiagnosticContactId(contact.userId)
                let relayHost = relayDiagnosticHost(usedConfig.relayUrl)
                relaySyncLog.warning(
                    "Contact \(contactId, privacy: .public) relay host=\(relayHost, privacy: .public) rejected us (\(String(describing: fault), privacy: .public), streak=\(streak, privacy: .public)); their friend card looks stale"
                )
            }
            /// Success is the only thing that clears a streak -- see
            /// `clear_contact_relay_rejection` for why a transient fault
            /// deliberately does not.
            func noteContactSuccess(contact: Contact, usedConfig: RelayConfig) {
                if let own = config,
                   usedConfig.relayUrl == own.relayUrl,
                   usedConfig.relayToken == own.relayToken { return }
                // The endpoint answering settles the silence question outright,
                // whatever this pass had provisionally observed.
                noteContactAnswered(contact)
                guard rejections[contact.userId] != nil else { return }
                try? store.clearContactRelayRejection(userId: contact.userId)
                rejections[contact.userId] = nil
                countedThisPass.remove(contact.userId)
            }
            // Recipients we already know we cannot post to on this pass. Core
            // excludes them in the queries below rather than the loops
            // skipping them afterwards: a row that is fetched and then skipped
            // has still consumed one of relayStoreBatchLimit slots, so
            // filtering downstream leaves the starvation intact. Both the
            // receipt queue and the outbound queue need it -- in the field
            // capture the receipt queue was the one visibly failing. Mirrors
            // RelaySyncEngine.kt.
            // C2 canary: begin capturing this legacy pass if it is one of the
            // sampled ones. Everything below feeds values, never a live object;
            // `finishPass` runs in the `defer` so it fires whether the pass
            // returns or throws, and after the uploads, so nothing it does can
            // change what the pass did. A pass with no rows to compare spends no
            // sample and writes nothing.
            let shadowCapture = relayShadowAdapter.beginPass(nowMs: now)
            let shadowContacts = contacts.map { contact in
                CoreRelayShadowContact(
                    userId: contact.userId,
                    relayUrl: contact.relayUrl,
                    relayToken: contact.relayToken,
                    endpointUsable: endpointUsable(contact) && endpointAnswering(contact)
                )
            }
            defer {
                relayShadowAdapter.finishPass(
                    capture: shadowCapture,
                    own: config,
                    contacts: shadowContacts,
                    nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
                )
            }
            let skipRecipients = contacts
                .filter { sendConfig(for: $0) == nil }
                .map { $0.userId }
            shadowCapture?.noteSkippedRecipients(skipRecipients)
            if !skipRecipients.isEmpty {
                let skipped = skipRecipients.map { UserIdHex.encode($0) }.joined(separator: ", ")
                relaySyncLog.info(
                    "Skipping relay upload for \(skipRecipients.count, privacy: .public) unreachable recipient(s) this pass: \(skipped, privacy: .public)"
                )
            }
            let receipts = try store.pendingRelayOutgoingReceiptEnvelopes(
                limit: MeshDefaults.relayStoreBatchLimit,
                nowMs: now,
                skipRecipientUserIds: skipRecipients
            )
            for env in receipts {
                guard let contact = contactsById[env.recipientUserId],
                      let cfg = sendConfig(for: contact)
                else { continue }
                do {
                    _ = try relayRequest { try RelayClient.postReceiptEnvelope(config: cfg, envelope: env) }
                    _ = try store.markOutgoingReceiptEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                    noteContactSuccess(contact: contact, usedConfig: cfg)
                    shadowCapture?.noteSucceeded(
                        lane: .receipt, msgId: env.msgId, hopTtl: env.hopTtl,
                        recipientHint: env.recipientHint, recipientUserId: env.recipientUserId,
                        sealedLen: env.sealed.count, expiryMs: env.expiry, endpoint: cfg
                    )
                } catch {
                    try rethrowFamilyRateLimit(error)
                    noteFailure(error, usedConfig: cfg)
                    noteContactFailure(error, contact: contact, usedConfig: cfg)
                    shadowCapture?.noteFailed(
                        lane: .receipt, msgId: env.msgId, hopTtl: env.hopTtl,
                        recipientHint: env.recipientHint, recipientUserId: env.recipientUserId,
                        sealedLen: env.sealed.count, expiryMs: env.expiry, endpoint: cfg, error: error
                    )
                }
            }
            // A stranded outbound queue was previously invisible in a support
            // archive: "nothing is arriving" read the same whether the queue
            // was deep or empty. One lopsided recipient here is the signature
            // of a contact whose relay is unreachable. Mirrors
            // RelaySyncEngine.kt.
            let queueDepth = (try? store.pendingRelayOutboundDepthByRecipient(nowMs: now)) ?? []
            if !queueDepth.isEmpty {
                let depths = queueDepth
                    .map { "\(UserIdHex.encode($0.recipientUserId))=\($0.queued)" }
                    .joined(separator: ", ")
                relaySyncLog.info(
                    "Outbound relay queue depth by recipient: \(depths, privacy: .public)"
                )
            }
            let outbound = try store.pendingRelayOutboundEnvelopes(
                limit: MeshDefaults.relayStoreBatchLimit,
                nowMs: now,
                skipRecipientUserIds: skipRecipients
            )
            let importedGroups = try store.listGroups()
            let groupsById = Dictionary(
                uniqueKeysWithValues: importedGroups.map { ($0.id, $0) }
            )
            /// Which single mailbox a group envelope's fan-out rows go to, or
            /// nil for "post nothing this pass" -- which leaves the envelope
            /// queued for a later pass and for the BLE/LAN paths, exactly as
            /// the 1:1 skip does.
            ///
            /// The choice itself is core's because the rule that matters is
            /// easy to get subtly wrong in one shell: a member whose endpoint
            /// is *resting for silence* contributes no fallback to our own
            /// mailbox. Falling back for them would post a cross-family
            /// member's copy where they never read, and `relayPostedAt` is
            /// terminal, so that is a permanent misroute rather than a retry.
            /// A member written off for *rejection* still falls back,
            /// unchanged. Mirrors RelaySyncEngine.kt.
            func relayConfigForGroupRecipient(_ groupId: Data) -> RelayConfig? {
                guard let group = groupsById[groupId] else { return config }
                let members = group.memberUserIds.compactMap { member -> GroupRelayMember? in
                    guard let contact = contactsById[member] else { return nil }
                    return GroupRelayMember(
                        relayUrl: contact.relayUrl,
                        relayToken: contact.relayToken,
                        endpointUsable: endpointUsable(contact),
                        endpointAnswering: endpointAnswering(contact)
                    )
                }
                guard let target = coreGroupFanoutRelayTarget(
                    members: members,
                    fallbackUrl: config?.relayUrl,
                    fallbackToken: config?.relayToken
                ) else { return nil }
                return RelayConfig(relayUrl: target.url, relayToken: target.token)
            }
            for env in outbound {
                guard let contact = contactsById[env.recipientUserId] else {
                    guard let cfg = relayConfigForGroupRecipient(env.recipientUserId) else { continue }
                    guard let group = groupsById[env.recipientUserId] else {
                        // Recipient is neither contact nor imported group
                        // (e.g. a group deleted mid-queue); keep the legacy
                        // single post so the envelope isn't stranded. The canary
                        // cannot speak for it — core resolves a destination per
                        // contact, not per bare recipient id.
                        shadowCapture?.noteUnshadowed(1)
                        do {
                            _ = try relayRequest { try RelayClient.postOutboundEnvelope(config: cfg, envelope: env) }
                            _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                        } catch {
                            try rethrowFamilyRateLimit(error)
                            noteFailure(error, usedConfig: cfg)
                        }
                        continue
                    }
                    // Group-addressed: per-member fan-out instead of one
                    // shared group-hint row (specs/group-relay-durability.md
                    // §4.2). Mark relay-posted only after ALL member rows
                    // post; a partial failure retries the whole set next
                    // pass, and the deterministic fan-out msg_ids dedupe
                    // server-side. Mirrors RelaySyncEngine.kt.
                    let rows = coreGroupFanoutRows(
                        originalMsgId: env.msgId,
                        memberUserIds: group.memberUserIds,
                        hopTtl: env.hopTtl,
                        expiry: env.expiry,
                        sealed: env.sealed,
                        envelopeTimestampMs: env.timestamp
                    )
                    // A group-addressed row is a lane core does not decompose
                    // per member; counted so a clean report never reads as a
                    // claim about it.
                    shadowCapture?.noteUnshadowed(max(1, group.memberUserIds.count))
                    var posted = 0
                    for row in rows {
                        do {
                            _ = try relayRequest { try RelayClient.postFanoutRow(config: cfg, row: row) }
                            posted += 1
                        } catch {
                            try rethrowFamilyRateLimit(error)
                            noteFailure(error, usedConfig: cfg)
                        }
                    }
                    if posted == rows.count {
                        _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                    }
                    continue
                }
                guard let cfg = sendConfig(for: contact) else { continue }
                do {
                    _ = try relayRequest { try RelayClient.postOutboundEnvelope(config: cfg, envelope: env) }
                    _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                    noteContactSuccess(contact: contact, usedConfig: cfg)
                    shadowCapture?.noteSucceeded(
                        lane: .authored, msgId: env.msgId, hopTtl: env.hopTtl,
                        recipientHint: env.recipientHint, recipientUserId: env.recipientUserId,
                        sealedLen: env.sealed.count, expiryMs: env.expiry, endpoint: cfg
                    )
                } catch {
                    try rethrowFamilyRateLimit(error)
                    noteFailure(error, usedConfig: cfg)
                    noteContactFailure(error, contact: contact, usedConfig: cfg)
                    shadowCapture?.noteFailed(
                        lane: .authored, msgId: env.msgId, hopTtl: env.hopTtl,
                        recipientHint: env.recipientHint, recipientUserId: env.recipientUserId,
                        sealedLen: env.sealed.count, expiryMs: env.expiry, endpoint: cfg, error: error
                    )
                }
            }
            // Carried mail starves like the outbound and receipt queues: a
            // failed upload leaves the row unmarked, so under flat order one
            // unreachable destination refills the batch every pass. Core
            // resolves each row's rotating recipient hint to a contact so it
            // can partition and skip. Mirrors RelaySyncEngine.kt.
            // Lightweight sync continues on a constrained path, but carried
            // envelopes remain queued for a later relay or local delivery.
            // The rows are not even read on that path: skipping the query is
            // the point, and an explicit type keeps the empty case unambiguous.
            let family: [CarriedEnvelope]
            if relayNetworkVerdict(pathMonitor?.currentPath) == .deferredConstrained {
                family = []
            } else {
                family = try store.familyCarriedEnvelopes(
                    limit: MeshDefaults.relayStoreBatchLimit,
                    nowMs: now,
                    skipRecipientUserIds: skipRecipients
                )
            }
            for env in family {
                // Carried rows are a lane a later package owns; the canary
                // cannot speak for them, so each is counted as unshadowed.
                shadowCapture?.noteUnshadowed(1)
                // Carried mail goes to the mailbox its recipient actually
                // polls: a contact hint posts to that contact's resolved
                // relay; a group hint decomposes into per-member fan-out rows
                // (specs/group-relay-durability.md §4.2); unrecognizable
                // hints are skipped. A successful post stamps the row's
                // upload marker so the next pass offers the NEXT batch
                // instead of re-posting this one for its whole seven-day
                // life (see markCarriedEnvelopeRelayUploaded in core).
                // Mirrors RelaySyncEngine.kt.
                if let contact = (try? store.contactMatchingHint(hint: env.recipientHint, nowMs: now)) ?? nil {
                    guard let cfg = sendConfig(for: contact) else { continue }
                    do {
                        _ = try relayRequest { try RelayClient.postCarriedEnvelope(config: cfg, envelope: env) }
                        _ = try store.markCarriedEnvelopeRelayUploaded(msgId: env.msgId, relayUrl: cfg.relayUrl)
                        noteContactSuccess(contact: contact, usedConfig: cfg)
                    } catch {
                        try rethrowFamilyRateLimit(error)
                        noteFailure(error, usedConfig: cfg)
                        noteContactFailure(error, contact: contact, usedConfig: cfg)
                    }
                    continue
                }
                if let group = (try? store.groupMatchingHint(hint: env.recipientHint, nowMs: now)) ?? nil {
                    guard let cfg = relayConfigForGroupRecipient(group.id) else { continue }
                    let rows = coreGroupFanoutRowsForCarried(
                        originalMsgId: env.msgId,
                        memberUserIds: group.memberUserIds,
                        hopTtl: env.hopTtl,
                        expiry: env.expiry,
                        sealed: env.sealed
                    )
                    // Stamped only once EVERY fan-out row landed -- a partial
                    // batch re-posts whole next pass, and the deterministic
                    // fan-out ids dedupe the rows that did land.
                    var posted = 0
                    for row in rows {
                        do {
                            _ = try relayRequest { try RelayClient.postFanoutRow(config: cfg, row: row) }
                            posted += 1
                        } catch {
                            try rethrowFamilyRateLimit(error)
                            noteFailure(error, usedConfig: cfg)
                        }
                    }
                    if posted == rows.count, !rows.isEmpty {
                        _ = try? store.markCarriedEnvelopeRelayUploaded(msgId: env.msgId, relayUrl: cfg.relayUrl)
                    }
                    continue
                }
            }

            // Gaining a contact or a group widens the fetch-hint set, and
            // relayd's next_cursor only ever covers the hints we sent -- so
            // mail that arrived under a hint we did not have yet is already
            // *below* the frontier, where no sweep interval can reach it. Core
            // notices the change and drops the frontiers; the walks below then
            // start at 0. Cheap when nothing changed (one digest of the id
            // set, no rows touched), so it is safe to run every pass.
            // Mirrors RelaySyncEngine.kt.
            if (try? store.noteRelayHintSources(ownUserId: identity.userId)) == true {
                relaySyncLog.info(
                    "Hint sources changed; re-walking every relay mailbox from the start"
                )
            }

            // Fetch-side parity with RelaySyncEngine.kt: poll every distinct
            // mailbox we know about -- our own plus each contact's card
            // relay. Mail addressed to us doesn't always reach our own
            // mailbox (a sender may only have had a fallback config, or an
            // older build posted to its own family mailbox), so checking one
            // box quietly loses cross-token mail.
            let distinctConfigs = Self.distinctRelayConfigs(
                // Reading a mailbox that is not answering is the same waste as
                // posting to it, and there is no fallback on the poll path
                // either way, so a rested endpoint simply drops out of the set.
                contacts: contacts.filter { endpointAnswering($0) },
                fallback: config,
                rejections: rejections,
                nowMs: now
            )
            guard !distinctConfigs.isEmpty else {
                await MainActor.run {
                    MeshConnectivityStatus.shared.setRelayHealth(.noConfig)
                }
                return
            }
            let presenceHints: (Data, Int64) -> [Data] = { userId, timestamp in
                recentPresenceHintsFor(userId: userId, nowMs: timestamp)
            }
            let fetchHints = try store.relayFetchHints(ownUserId: identity.userId, nowMs: now)
            var anyRelaySucceeded = false
            var ownRelaySucceeded = config == nil
            // Distinct from ownRelaySucceeded, which starts true when there is
            // no own relay at all. Only a mailbox that actually answered
            // counts as proof this device has working internet, and only that
            // may license resting a contact's silent endpoint.
            var ownRelayAnswered = false
            // Set when any mailbox's walk hits its per-pass budget and has
            // more to fetch. The delay is armed only once the whole
            // multi-mailbox pass has finished -- scheduling it from inside the
            // loop could let the timer fire while a later config is still
            // running and collapse the continuation into an in-flight rerun.
            // Mirrors RelaySyncEngine.kt's `mailboxContinuationNeeded`.
            var mailboxContinuationNeeded = false
            // The mailbox walk itself lives in `RelayMailboxWalk`, because it
            // was the one part of this pass with no test: it needed a live
            // relay and the whole controller to run at all, so the composition
            // of the core's walk rules -- where the #270 sweep livelock
            // actually lived -- could only be checked by reading it. Everything
            // pass-shaped (the request pacer, the family rate-limit abort, the
            // relay-health bookkeeping) stays on this side of
            // `RelayMailboxPages`. Mirrors RelaySyncEngine.kt.
            let mailboxWalk = RelayMailboxWalk(store: store) { env, walkIdentity in
                await self.onMeshQueue {
                    MeshController.shared.processInboundEnvelope(
                        sourceAddress: nil,
                        msgId: env.msgId,
                        hopTtl: env.hopTtl,
                        expiry: env.expiryMs,
                        recipientHint: env.recipientHint,
                        sealed: env.sealed,
                        identity: walkIdentity
                    )
                }
            }
            let mailboxPages = RelayMailboxPages(
                fetch: { walkConfig, hints, afterId, limit in
                    try relayRequest {
                        try RelayClient.fetchEnvelopesWithinResponseCap(
                            config: walkConfig,
                            hints: hints,
                            afterId: afterId,
                            limit: limit
                        ) { tried, smaller in
                            relaySyncLog.warning(
                                "Relay page was too big to take at limit=\(tried, privacy: .public); retrying with limit=\(smaller, privacy: .public)"
                            )
                        }
                    }
                },
                ack: { walkConfig, ids in
                    try relayRequest { try RelayClient.ackEnvelopes(config: walkConfig, ids: ids) }
                },
                abortsPass: { $0 is FamilyRelayRateLimitAbort },
                noteFailure: { walkConfig, error in noteFailure(error, usedConfig: walkConfig) },
                // The walk asks for this only after the store reports it
                // lowered this mailbox's frontier. A socket subscribed at the
                // old value can never deliver a row at or below it, so a
                // lowering that does not reach the socket leaves the live path
                // deaf to the whole rebuilt mailbox; `resubscribe` is a no-op
                // for every mailbox except the one this client watches.
                reopenPushSocket: { [self] walkConfig in
                    meshQueue.async { [self] in
                        relayPushClient.resubscribe(config: walkConfig)
                    }
                }
            )
            for cfg in distinctConfigs {
                // Presence rides each mailbox for the contacts resolved to
                // it; own presence is announced everywhere so contacts on
                // other relays still see us (mirrors syncRelayPresence in
                // RelaySyncEngine.kt). Presence failure is never fatal.
                // CP4: presence is a read, so contacts group under their
                // *poll* config — a family member's deposit-token card
                // resolves back to our own member config and their presence
                // keeps flowing through it.
                let contactsForConfig = contacts.filter { contact in
                    guard let resolved = Self.resolvedPollRelayConfig(contact: contact, fallback: config)
                    else { return false }
                    return resolved.relayUrl == cfg.relayUrl && resolved.relayToken == cfg.relayToken
                }
                if !contactsForConfig.isEmpty {
                    let announce = RelayConfigStore.shareOnline()
                        ? presenceHints(identity.userId, now)
                        : []
                    let query = Array(Set(contactsForConfig.flatMap { presenceHints($0.userId, now) }))
                    if !announce.isEmpty || !query.isEmpty {
                        let contactByHint = Dictionary(uniqueKeysWithValues: contactsForConfig.flatMap { contact in
                            presenceHints(contact.userId, now).map { ($0, contact.userId) }
                        })
                        do {
                            let page = try relayRequest {
                                try RelayClient.syncPresence(
                                    config: cfg,
                                    announce: announce,
                                    query: query
                                )
                            }
                            let localNow = Int64(Date().timeIntervalSince1970 * 1_000)
                            // The store write stays off the main actor (it was
                            // only ever inside the hop because the published
                            // merge next to it needed one); only the published
                            // merge itself is handed over.
                            var seen: [(Data, Int64)] = []
                            for item in page.presence {
                                guard let userId = contactByHint[item.hint] else { continue }
                                let localSeenAt = localNow - max(0, page.nowMs - item.lastSeenMs)
                                seen.append((userId, localSeenAt))
                                try? store.recordPeerConnectionEvent(
                                    userId: userId,
                                    transport: .shorePass,
                                    kind: .presenceSeen,
                                    occurredAtMs: localSeenAt
                                )
                            }
                            if !seen.isEmpty {
                                let merged = seen
                                await MainActor.run {
                                    for (userId, seenAtMs) in merged {
                                        MeshConnectivityStatus.shared.mergePresenceLastSeen(
                                            userId: userId,
                                            seenAtMs: seenAtMs
                                        )
                                    }
                                }
                            }
                        } catch {
                            try rethrowFamilyRateLimit(error)
                            noteFailure(error, usedConfig: cfg)
                        }
                    }
                }
                // Walk this mailbox: where the pass starts, when the
                // cursors may move, when the budget yields and when a sweep is
                // complete all live in `RelayMailboxWalk` (and, under it, in
                // core/src/relay_cursor.rs). It throws for the same reasons the
                // rest of this loop's body did, so the catch below is unchanged.
                do {
                    let walk = try await mailboxWalk.walk(
                        config: cfg,
                        identity: identity,
                        fetchHints: fetchHints,
                        nowMs: now,
                        pages: mailboxPages
                    )
                    if walk.continuationNeeded { mailboxContinuationNeeded = true }
                    anyRelaySucceeded = true
                    if let own = config, cfg.relayUrl == own.relayUrl, cfg.relayToken == own.relayToken {
                        ownRelaySucceeded = true
                        if walk.answered { ownRelayAnswered = true }
                    }
                } catch {
                    try rethrowFamilyRateLimit(error)
                    // A contact can carry stale relay credentials from an
                    // older friend card. That mailbox failing must not abort
                    // polling of the remaining relays or declare our own
                    // configured relay unreachable when it succeeded.
                    noteFailure(error, usedConfig: cfg)
                }
            }
            // Now that the pass knows whether our own mailbox answered, this
            // pass's silent contact endpoints can be judged -- or discarded.
            // `ownRelayAnswered` is handed straight to the core rather than
            // tested here: whether same-pass proof of working internet is
            // required, and what its absence means, is one rule both shells
            // must answer identically, so coreContactRelayUnreachableDelta is
            // the only place it is decided. Without the proof nothing is
            // recorded -- a phone in a tunnel fails every endpoint at once,
            // and resting them all would take the relay path away from every
            // contact for the whole rest window. Mirrors RelaySyncEngine.kt's
            // commitUnreachableContactRelays.
            for rested in ContactRelaySilence.shared.commitPass(
                otherRelayAnswered: ownRelayAnswered,
                nowMs: now
            ) {
                guard let contact = contactsById[rested.userId] else { continue }
                let key = endpointKey(contact)
                guard let streak = try? store.noteContactRelayUnreachable(
                    userId: contact.userId,
                    endpointKey: key,
                    nowMs: now
                ) else { continue }
                unreachable[contact.userId] = ContactRelayUnreachable(
                    userId: contact.userId,
                    endpointKey: key,
                    unreachableStreak: streak,
                    unreachableAtMs: now
                )
                let contactId = relayDiagnosticContactId(rested.userId)
                let relayHost = relayDiagnosticHost(contact.relayUrl)
                relaySyncLog.warning(
                    "Contact \(contactId, privacy: .public) relay host=\(relayHost, privacy: .public) did not answer while our own did (silent passes=\(streak, privacy: .public)); resting it rather than retrying every pass"
                )
            }
            familyRelayBackoff.onSuccessfulPass()
            if mailboxContinuationNeeded {
                // The controller's own state, read on `meshQueue`, so it is
                // written there too -- the same rule `relayRateLimitedUntilMs`
                // follows below.
                meshQueue.async { self.scheduleMailboxContinuation() }
            }
            let syncedAtMs = Int64(Date().timeIntervalSince1970 * 1_000)
            let fault = ownRelayFault
            let retryAfterMs = familyRetryDelayMs
            let ownSucceeded = ownRelaySucceeded
            let anySucceeded = anyRelaySucceeded
            // Reported from the streak alone, not from either current
            // usability probe: a relay stays reported stale while a periodic
            // probe is due, so the explanation does not blink out merely
            // because one request is temporarily permitted.
            let stale = Set(
                rejections.values
                    .filter { coreContactRelayIsStale(rejectStreak: $0.rejectStreak) }
                    .map(\.userId)
                + unreachable.values
                    .filter { coreContactRelayUnreachableIsStale(unreachableStreak: $0.unreachableStreak) }
                    .map(\.userId)
            )
            await MainActor.run {
                MeshConnectivityStatus.shared.setRelayHealth(RelayHealth.afterSyncPass(
                    fault: fault,
                    ownRelaySucceeded: ownSucceeded,
                    anyRelaySucceeded: anySucceeded,
                    nowMs: syncedAtMs
                ))
                MeshConnectivityStatus.shared.setStaleRelayContacts(stale)
            }
            // `relayRateLimitedUntilMs` is the controller's own state, read by
            // `runRelaySync` on `meshQueue`, so it is written there too.
            meshQueue.async {
                self.noteRelayRateLimit(fault: fault, retryAfterMs: retryAfterMs)
            }
        } catch {
            if let config { noteFailure(error, usedConfig: config) }
            let message: String
            if let rateLimit = error as? FamilyRelayRateLimitAbort {
                message = "Family relay rate limit halted pass; retrying in \(rateLimit.retryDelayMs)ms"
            } else {
                message = error.localizedDescription
            }
            let fault = ownRelayFault
            let retryAfterMs = familyRetryDelayMs
            await MainActor.run {
                let nowMs = Int64(Date().timeIntervalSince1970 * 1_000)
                MeshConnectivityStatus.shared.setRelayHealth(RelayHealth.afterSyncPass(
                    fault: fault,
                    ownRelaySucceeded: false,
                    anyRelaySucceeded: false,
                    nowMs: nowMs
                ))
            }
            meshQueue.async {
                self.noteRelayRateLimit(fault: fault, retryAfterMs: retryAfterMs)
            }
            relaySyncLog.warning("Relay sync failed: \(message, privacy: .public)")
        }
    }

    /// **§10 step 2, driven from the pass that owns the network.**
    ///
    /// A device removal writes the rotation down and returns immediately; this
    /// is where it actually happens, for the same reason every other relay call
    /// lives on this side of the seam. Failure never touches the pass: a
    /// rotation is a repair the fleet owes itself, not a precondition for moving
    /// mail, and a relay that refuses to re-key must not also stop messages
    /// being delivered. Mirrors Android's
    /// `RelaySyncEngine.rotateFamilyTokenIfOwed`.
    ///
    /// Returns whether this device's saved pass moved, which is the one thing
    /// the rest of the pass has to know: the credential it captured a moment
    /// ago is the one the relay has just retired.
    private func rotateFamilyTokenIfOwed(identity: Identity) -> Bool {
        let driver = RelayRotationDriver()
        // Both directions of §10.2's own-device leg, in the order that matters:
        // a device that was told about a rotation writes it down before it
        // could waste an attempt asking about one of its own.
        let adopted = driver.adoptAnnouncedCredential()
        if case .rotated = driver.rotateIfPending(identity: identity) { return true }
        return adopted
    }

    /// The relay sync pass, core engine (C2). Mirrors Android
    /// `RelaySyncEngine.performCoreRelaySyncPass`.
    ///
    /// Assembles the facts core cannot read from the store — this device's own
    /// pass, its contacts' cards, the hints it fetches under, whether its
    /// endpoint changed, and the quiet window already in force — hands them over
    /// as a `CoreRelayPassPlan`, drives the actions that come back through
    /// `RelaySyncDriver`/`RelayActionDriver`, and projects the summary onto the
    /// health pill and the retry/continuation timers this shell owns. It makes
    /// no protocol arithmetic itself.
    ///
    /// Off by default; `RelayEngineSettings.passEngine` selects it. The
    /// remaining known gap keeping the default legacy is group fan-out,
    /// recorded in the contract's §5.2. What used to sit beside it, and no
    /// longer does: an ingested page now reaches the inbound processor through
    /// `CoreRelayPassProjector`, so it raises the same notification the legacy
    /// walk raises; a presence answer now reaches `MeshConnectivityStatus` the
    /// same way; and a contact endpoint resting for *silence* is now told apart
    /// from one that was rejected, because the plan below carries both brakes
    /// rather than folding them into one. What the shell still owns on both
    /// paths — the receipt backfill and the announce — already ran in
    /// `runRelaySync` before this is reached, so nothing here re-runs them.
    private func performCoreRelaySyncPass(identity: Identity, config: RelayConfig?) async {
        let store = AppStore.get()
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let contacts = (try? store.listContacts()) ?? []
        let rejections = Dictionary(
            uniqueKeysWithValues: ((try? store.listContactRelayRejections()) ?? []).map { ($0.userId, $0) }
        )
        let unreachable = (try? store.listContactRelayUnreachable()) ?? []
        // The silence breaker is state this shell keeps and core's
        // `endpoint_usable` reads; restore it and open the pass boundary exactly
        // as the legacy pass does, or every rested endpoint answers "still
        // answering" and a retired host is re-dialled on every pass.
        ContactRelaySilence.shared.restore(unreachable)
        ContactRelaySilence.shared.beginPass()
        func endpointUsable(_ contact: Contact) -> Bool {
            Self.contactEndpointUsable(contact: contact, rejections: rejections, nowMs: now)
        }
        func endpointAnswering(_ contact: Contact) -> Bool {
            ContactRelaySilence.shared.endpointAnswering(
                userId: contact.userId,
                endpointKey: relayCursorKey(
                    relayUrl: contact.relayUrl ?? "",
                    relayToken: contact.relayToken ?? ""
                ),
                nowMs: now
            )
        }
        let anyConfigKnown = config != nil || contacts.contains {
            Self.resolvedPollRelayConfig(contact: $0, fallback: config) != nil
        }
        guard anyConfigKnown else {
            await MainActor.run { MeshConnectivityStatus.shared.setRelayHealth(.noConfig) }
            return
        }
        let fetchHints = (try? store.relayFetchHints(ownUserId: identity.userId, nowMs: now)) ?? []
        let presenceAnnounce = RelayConfigStore.shareOnline()
            ? recentPresenceHintsFor(userId: identity.userId, nowMs: now)
            : []
        let presenceQuery = contacts.flatMap { recentPresenceHintsFor(userId: $0.userId, nowMs: now) }
        // Read before any announce records the epoch as sent; `runRelaySync`
        // already queued the notice for both engines, so this only tells core's
        // announce stage whether the shell has already fanned the endpoint out.
        let ownEndpointChanged = RelayConfigStore.relayEpoch() > RelayConfigStore.announcedRelayEpoch()

        let plan = CoreRelayPassPlan(
            own: config.map { CoreRelayEndpointConfig(url: $0.relayUrl, token: $0.relayToken) },
            contacts: contacts.map { contact in
                CoreRelayContactConfig(
                    userId: contact.userId,
                    relayUrl: contact.relayUrl,
                    relayToken: contact.relayToken,
                    // Two brakes, kept apart. `endpointUsable` is rejection
                    // evidence — the endpoint answered and refused us — and an
                    // upload for such a contact falls back to this device's own
                    // mailbox. `endpointAnswering` is silence evidence — nothing
                    // answered at all — and an upload for one is declined
                    // outright this pass instead, because a host that has merely
                    // gone quiet is not proof that somebody else's mailbox is
                    // the right place to leave their mail. Folded into one flag,
                    // as this call did, every rested endpoint borrowed the
                    // rejection answer and took the fallback.
                    endpointUsable: endpointUsable(contact),
                    endpointAnswering: endpointAnswering(contact)
                )
            },
            ownUserId: identity.userId,
            fetchHints: fetchHints,
            presenceAnnounce: presenceAnnounce,
            presenceQuery: presenceQuery,
            ownEndpointChanged: ownEndpointChanged,
            sweptThisSession: coreEngineSweptThisSession,
            consecutiveRateLimits: UInt32(clamping: familyRelayBackoff.consecutiveRateLimits),
            quietUntilMs: relayRateLimitedUntilMs,
            budgets: coreRelayPassDefaultBudgets()
        )

        let identityPublicBytes = identity.userId
        let executor = LiveRelayActionExecutor(
            isCancelled: { [weak self] in !(self?.isRunning ?? false) },
            pace: { [weak self] in
                guard let self else { return }
                // Monotonic on purpose: the pacer's reservation must not be
                // rewound by a wall-clock correction. The budget belongs to the
                // family's token, not to whichever engine spends it.
                let monotonicNowMs = Int64(clamping: DispatchTime.now().uptimeNanoseconds / 1_000_000)
                let waitMs = self.familyRelayRequestPacer.reserve(nowMs: monotonicNowMs)
                if waitMs > 0 { Thread.sleep(forTimeInterval: Double(waitMs) / 1_000) }
            }
        )
        // The same inbound call the legacy walk makes, so a notification, a chat
        // row and a receipt look identical whichever engine fetched the
        // envelope; and the same "last seen" merge the legacy presence sync
        // makes. Delivery is enqueued on `meshQueue` rather than awaited: it
        // serialises with BLE and LAN frames exactly as the legacy walk's
        // envelopes do, and nothing here needs the disposition back — core
        // already committed the ack decision inside its own transaction.
        let projector = CoreRelayPassProjector(
            deliver: { [weak self] envelope, passIdentity in
                self?.meshQueue.async {
                    _ = MeshController.shared.processInboundEnvelope(
                        sourceAddress: nil,
                        msgId: envelope.msgId,
                        hopTtl: envelope.hopTtl,
                        expiry: envelope.expiryMs,
                        recipientHint: envelope.recipientHint,
                        sealed: envelope.sealed,
                        identity: passIdentity
                    )
                }
            },
            mergePresence: { userId, seenAtMs in
                Task { @MainActor in
                    MeshConnectivityStatus.shared.mergePresenceLastSeen(userId: userId, seenAtMs: seenAtMs)
                }
            },
            notePresenceSeen: { userId, seenAtMs in
                try? store.recordPeerConnectionEvent(
                    userId: userId,
                    transport: .shorePass,
                    kind: .presenceSeen,
                    occurredAtMs: seenAtMs
                )
            }
        )
        let summary = RelaySyncDriver(
            store: store,
            executor: executor,
            clock: { Int64(Date().timeIntervalSince1970 * 1_000) },
            isCancelled: { [weak self] in !(self?.isRunning ?? false) },
            onProjection: { projection in
                projector.project(
                    projection,
                    identity: identity,
                    contacts: contacts,
                    nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
                )
            }
        ).run(plan: plan, passId: "cp")

        // Only a pass that ran every stage has swept every mailbox; a cancelled,
        // refused or rate-limited pass swept nothing.
        if summary.outcome == .completed { coreEngineSweptThisSession = true }

        let syncedAtMs = Int64(Date().timeIntervalSince1970 * 1_000)
        let health = Self.relayHealth(forCorePass: summary.health, nowMs: syncedAtMs)
        let outcome = summary.outcome
        let quietUntilMs = summary.quietUntilMs
        let continuationNeeded = summary.continuation != nil
        relaySyncLog.info(
            "Core relay pass complete: outcome=\(String(describing: outcome), privacy: .public) requests=\(summary.requestsIssued, privacy: .public) ingested=\(summary.rowsIngested, privacy: .public)"
        )
        await MainActor.run {
            MeshConnectivityStatus.shared.setRelayHealth(health)
        }
        // `relayRateLimitedUntilMs` and the backoff counter are the controller's
        // own state, read by `runRelaySync` on `meshQueue`, so they are written
        // there too. `RATE-01`'s escalation moves on both sides of a core pass:
        // only a completed pass clears it, only a rate-limited one bumps it.
        meshQueue.async { [weak self] in
            guard let self else { return }
            switch outcome {
            case .rateLimited:
                // For the count alone; core already decided the window this
                // refusal earns and put it in the summary.
                _ = self.familyRelayBackoff.onRateLimited(retryAfterMs: 0, identityPublicBytes: identityPublicBytes)
                if quietUntilMs > 0 {
                    self.relayRateLimitedUntilMs = max(self.relayRateLimitedUntilMs, quietUntilMs)
                }
            case .completed:
                self.familyRelayBackoff.onSuccessfulPass()
                if quietUntilMs <= Int64(Date().timeIntervalSince1970 * 1_000) {
                    self.relayRateLimitedUntilMs = 0
                }
            default:
                break
            }
            let quietRemainingMs = self.relayRateLimitedUntilMs - Int64(Date().timeIntervalSince1970 * 1_000)
            if quietRemainingMs > 0 {
                self.scheduleRelayRateLimitRetry(afterMs: quietRemainingMs)
            } else if continuationNeeded {
                self.scheduleMailboxContinuation()
            }
        }
    }

    /// Projects the core pass's own health onto this shell's display type,
    /// attaching the shell's clock reading. The precedence itself is core policy
    /// (`coreRelayPassHealth`, `RATE-01`); this is the same projection
    /// `RelayHealth.afterSyncPass` does, applied to a health core already folded.
    private static func relayHealth(forCorePass health: CoreRelayPassHealth, nowMs: Int64) -> RelayHealth {
        switch health {
        case .ok: return .ok(lastSyncMs: nowMs)
        case .quotaFull: return .quotaFull(lastAttemptMs: nowMs)
        case .messageTooLarge: return .messageTooLarge(lastAttemptMs: nowMs)
        case .rateLimited: return .rateLimited(lastAttemptMs: nowMs)
        case .expired: return .expired(lastAttemptMs: nowMs)
        case .expiredReadOnly: return .expiredReadOnly(lastAttemptMs: nowMs)
        case .suspended: return .suspended(lastAttemptMs: nowMs)
        case .tokenRejected: return .tokenRejected(lastAttemptMs: nowMs)
        case .failing: return .failing(lastAttemptMs: nowMs)
        }
    }

    /// Resume a bounded mailbox walk shortly after the pass that yielded it.
    ///
    /// The delay comes from the core (`relayMailboxContinuationDelayMs`), so
    /// both shells hand the phone back for the same length of time. Exactly
    /// one continuation is ever outstanding: a later yield replaces the
    /// pending one rather than stacking passes up behind each other, and
    /// `runRelaySync` coalesces a continuation that lands while a pass is
    /// still in flight. Mirrors RelaySyncEngine.kt's
    /// `scheduleMailboxContinuation`.
    private func scheduleMailboxContinuation() {
        relayMailboxContinuationWorkItem?.cancel()
        let resume = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.relayMailboxContinuationWorkItem = nil
            self.runRelaySync()
        }
        relayMailboxContinuationWorkItem = resume
        meshQueue.asyncAfter(
            deadline: .now() + .milliseconds(Int(clamping: relayMailboxContinuationDelayMs())),
            execute: resume
        )
    }

    /// Remember (or clear) the family quiet window and keep exactly one retry
    /// scheduled at its end. `runRelaySync` coalesces every earlier nudge.
    private func noteRelayRateLimit(fault: CoreRelayFault?, retryAfterMs: UInt64) {
        if fault == .rateLimited {
            relayRateLimitedUntilMs = Int64(Date().timeIntervalSince1970 * 1_000) + Int64(retryAfterMs)
            scheduleRelayRateLimitRetry(afterMs: Int64(clamping: retryAfterMs))
        } else {
            relayRateLimitedUntilMs = 0
            relayRateLimitRetryWorkItem?.cancel()
            relayRateLimitRetryWorkItem = nil
        }
    }

    /// Arms the one coalesced retry that ends a quiet window. Exactly one is
    /// ever outstanding, whatever number of nudges arrived during the window --
    /// that coalescing is the whole point of `RATE-01`'s deferral, and it is
    /// why a pending rerun is cheap to defer and expensive to honour.
    ///
    /// `afterMs` may be zero or negative if the window has already elapsed, in
    /// which case the retry runs at the next opportunity rather than never.
    private func scheduleRelayRateLimitRetry(afterMs: Int64) {
        relayRateLimitRetryWorkItem?.cancel()
        let retry = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.relayRateLimitRetryWorkItem = nil
            self.runRelaySync()
        }
        relayRateLimitRetryWorkItem = retry
        meshQueue.asyncAfter(
            deadline: .now() + .milliseconds(Int(clamping: max(afterMs, 0))),
            execute: retry
        )
    }

    /// Records that a friend's own message landed on this phone, for the
    /// Connection details screen.
    ///
    /// Deliberately narrow. Only kinds a person actually sees in a
    /// conversation count (`isVisibleChatKind`) -- receipts, profile sync,
    /// relay updates, endpoint hints, reactions and every other hidden kind
    /// are machine chatter and would make the screen claim a friend had
    /// written when nobody did. Unknown senders are skipped too: the screen
    /// only lists friends, so an event for anyone else could never be shown
    /// against a name.
    ///
    /// Best-effort: connection history is a diagnostic, never worth failing a
    /// real message delivery over.
    private func recordInboundChatArrival(
        senderUserId: Data,
        kind: UInt8,
        arrival: MessageArrival?
    ) {
        guard isVisibleChatKind(kind), let arrival else { return }
        guard (try? store.getContact(userId: senderUserId)) != nil else { return }
        // The message is already durably stored, so this arrival really
        // happened: latch it if it came in without the internet. That is the
        // proof the checklist's "send a message with no internet" step waits
        // for, and this is the only place on the phone where the fact is
        // observed at all. Written once and never withdrawn.
        OfflineDeliverySeenStore.noteArrival(transport: arrival.transport)
        try? store.recordPeerConnectionEvent(
            userId: senderUserId,
            transport: corePeerTransportForArrival(transport: arrival.transport),
            kind: .messageReceived,
            occurredAtMs: arrival.receivedAt
        )
    }

    private func recordPeerDisconnected(address: String) {
        // The link's byte allowance goes with the link. The *peer's* cadence
        // deliberately does not: dropping it here is exactly how reconnect
        // churn would reset the gate that exists for reconnect churn.
        SprayPolicy.noteLinkClosed(address: address)
        guard let userId = MeshRouter.userIdFor(address: address),
              let transport = MeshRouter.transportFor(address: address),
              (try? store.getContact(userId: userId)) != nil else { return }
        recordPeerConnection(userId: userId, transport: transport, kind: .disconnected)
    }

    private func recordPeerConnection(
        userId: Data,
        transport: MeshRouterState.Transport,
        kind: PeerConnectionEventKind
    ) {
        let path: PeerConnectionTransport = transport == .lan ? .localWifi : .bluetooth
        try? store.recordPeerConnectionEvent(
            userId: userId,
            transport: path,
            kind: kind,
            occurredAtMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )
    }

    /// Both stores are main-actor `ObservableObject`s driving the UI, so this
    /// is the pipeline's one purely-UI hop. `MeshRouter` is the source of
    /// truth for both values and is readable from the main actor, so the route
    /// snapshot is deliberately taken there rather than passed across: taking
    /// it here and handing it over would let a later mesh event's hop overtake
    /// an earlier one's data. `DispatchQueue.main.async` preserves the order
    /// these are emitted in.
    private func refreshNearby() {
        guard isRunning else { return }
        onMain {
            MeshConnectivityStatus.shared.refreshNearbyRoutes()
            MeshRuntimeStatus.shared.markMeshing(nearby: MeshRouter.connectedUserCount())
        }
    }
}

/// Self + owned-group recipient hints for the current moment -- the same hint
/// set `MeshController.relaySyncBlocking` computes inline for its own relay
/// fetch. A free function (not a `MeshController` method) because
/// `RelayPushClient` (DTN_TODOS.md D3) invokes its `hintsProvider` closure
/// from its own private queue -- neither the main thread nor the controller's
/// mesh queue -- so it must reach nothing but the store, which is safe from
/// any thread.
/// FI2: unlike this function, `relaySyncBlocking`'s own fetch ALSO includes
/// `relayProxyHints` below (mail addressed to a contact, fetched on their
/// behalf) -- deliberately not mirrored here. This hint set only decides
/// which relay topics wake the push socket/reconnect; the proxy hint set
/// scales with contact-list size (one subscription per contact per
/// recent day), and the 60s poll already covers proxy-fetched mail without
/// needing a push nudge for it. Revisit if proxy-fetch latency ever needs to
/// beat the poll interval.
///
/// Uses `relaySelfPushHints`, not `relaySelfHints`: the push subscription is
/// computed once per socket connect and the socket then stays open
/// indefinitely (relayd keepalive pings), so a socket opened before the UTC
/// day rollover would otherwise subscribe only to hints that stop matching
/// anything the moment the day-salt rotates. `relaySelfPushHints` adds one
/// day ahead for the same ids so the subscription survives the rollover;
/// see its doc for why that's safe (it only widens what the subscription
/// matches -- envelopes are still ever created with a backward-looking
/// hint).
private func relayPushHints(ownUserId: Data) -> [Data] {
    let store = AppStore.get()
    let now = Int64(Date().timeIntervalSince1970 * 1000)
    // On a store error fall back to just our own hints, matching the old
    // inline loop.
    return (try? store.relaySelfPushHints(ownUserId: ownUserId, nowMs: now))
        ?? recentHintsFor(userId: ownUserId, nowMs: now)
}

/// The full `/ws` subscribe: `relayPushHints` plus the poll path's persisted
/// fetch frontier for this relay, so a reconnect asks relayd to replay from
/// there rather than from 0 (which replayed the entire mailbox as frames the
/// doorbell then discarded). Free function for the same reason
/// `relayPushHints` is one: `RelayPushClient` invokes it off the main actor.
private func relayPushSubscription(ownUserId: Data, config: RelayConfig) -> RelayPushSubscription {
    let store = AppStore.get()
    let cursorKey = relayCursorKey(relayUrl: config.relayUrl, relayToken: config.relayToken)
    let afterId = (try? store.relayFetchCursor(configKey: cursorKey))?.afterId ?? 0
    return RelayPushSubscription(hints: relayPushHints(ownUserId: ownUserId), afterId: afterId)
}

