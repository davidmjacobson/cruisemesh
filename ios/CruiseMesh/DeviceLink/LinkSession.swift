import Foundation
import os.log

/// How the two phones reach each other (`specs/multi-device-v1.md` §9.2).
/// Hashable, not merely Equatable: the transport disclosure renders it as a
/// SwiftUI `Picker`, whose `tag` requires it.
enum LinkTransport: Hashable { case lan, relay }

/// How far one run has got. Rendered from localized copy, never from here.
enum LinkStep: Equatable {
    case idle
    case waitingForPeer
    case handshaking
    case comparingDigits
    case carryingBootstrap
    case activating
    case done
    case failed
}

/// What landed, for the sentence the person reads when the run finishes.
struct LinkReport: Equatable {
    let deviceIdHex: String
    let rosterHeadHex: String
    let rosterSeq: UInt64
    let contacts: UInt32
    let groups: UInt32
    let messages: UInt32
    let catchUpChats: Int
}

struct LinkState: Equatable {
    var role: CoreLinkRole?
    var transport: LinkTransport?
    var step: LinkStep = .idle
    var qrText: String?
    var sas: String?
    var confirmHere = false
    var warnSoftCap = false
    var outcome: CoreLinkOutcome?
    var report: LinkReport?
    /// Raw diagnostic text. Developer-facing, never family-facing copy.
    var failure: String?
}

/// One end-to-end run of §9: the ceremony a family actually performs.
///
/// The Swift twin of Android's `LinkSession.kt` — the same core objects, the same
/// ordering, the same refusals, the same two greppable "link complete" log lines
/// the §13 WP6 smoke gate reads.
///
/// # What it deliberately does not do
///
/// It never touches `IdentityStore`. A linked device receives the person's
/// *inbox* key in the bootstrap and never the person root signing secret, which
/// §14.2 keeps inside the encrypted backup — so there is no complete `Identity`
/// for a new device to adopt, and inventing one here would be inventing the thing
/// the spec spent §3 taking away. Authoring as the person from a second device is
/// WP4's per-device author streams; until then a newly linked phone reads what it
/// was given and stays quiet.
///
/// - Parameter expectedPersonId: the person the new device believes it is
///   joining, read out of the `.cmbak` the restore flow opened (§9's closing
///   paragraph). Core refuses a bootstrap for anybody else. `nil` means "whoever
///   adopts this factory-fresh phone", which is all the ceremony can promise when
///   no backup was opened first.
final class LinkSession: ObservableObject {
    @Published private(set) var state = LinkState()

    private let identity: Identity
    private let expectedPersonId: Data?
    private let store: MessageStore
    private let log = Logger(subsystem: "com.cruisemesh", category: "LinkSession")

    private let flags = LinkSessionFlags()
    private var worker: Thread?

    private static let receiveWaitMs: Int64 = 1_000
    private static let connectTimeoutMs: Int64 = 5_000
    /// How long the §9.3-§9.4 exchange may take after the digits matched.
    /// Separate from the ceremony's own deadline, which covers the part a person
    /// is standing in front of.
    private static let bootstrapDeadlineMs: Int64 = 120_000
    /// How long to wait for the mesh controller to actually take the radios down
    /// after the §9.4 window opens. Failing it aborts the ceremony rather than
    /// proceeding on a phone that is still advertising.
    private static let visibilityApplyTimeoutMs: Int64 = 5_000

    init(identity: Identity, expectedPersonId: Data? = nil, store: MessageStore = AppStore.get()) {
        self.identity = identity
        self.expectedPersonId = expectedPersonId
        self.store = store
    }

    /// Whether this phone may be adopted as a new device at all (§9.3).
    ///
    /// Asked before a ceremony starts rather than after one, because the answer
    /// cannot change during it and the alternative is a person holding two
    /// phones together, comparing six digits, and only then being told this one
    /// was never eligible.
    func importReadiness() -> CoreLinkImportReadiness {
        do {
            return try store.linkImportReadiness(expectedPersonId: expectedPersonId)
        } catch {
            log.warning("Could not read link import readiness: \(String(describing: error), privacy: .public)")
            return .storeHoldsSomeone
        }
    }

    /// §9.1: mint an offer, show it, and wait to be adopted.
    func startAsNewDevice(transport: LinkTransport) {
        launch(role: .newDevice, transport: transport) { [weak self] in
            try self?.runNewDevice(transport: transport)
        }
    }

    /// §9.2: scan an offer and, if the digits match, adopt what showed it.
    func startAsApprovingDevice(qrText: String, transport: LinkTransport) {
        launch(role: .approvingDevice, transport: transport) { [weak self] in
            try self?.runApprovingDevice(qrText: qrText, transport: transport)
        }
    }

    /// §9.2's explicit action. Only ever reachable on the approving device.
    func answerDigits(matches: Bool) {
        flags.setDecision(matches ? .matches : .doesNotMatch)
    }

    func cancel() { flags.requestCancel() }

    func close() {
        cancel()
        worker?.cancel()
    }

    /// Back to the start after a run has ended, so "start over" is one tap
    /// rather than leaving the screen and coming back in by the same door.
    ///
    /// Only ever reached from an ended run: the screen offers it beside "Done",
    /// on the step a worker writes as its last act before returning. That is
    /// what makes dropping the `worker` reference safe here and nowhere else —
    /// `launch`'s "one run at a time" guard reads it, so clearing it mid-run
    /// would let a second ceremony start underneath the first.
    func reset() {
        close()
        worker = nil
        flags.reset()
        publish { $0 = LinkState() }
    }

    // -----------------------------------------------------------------------

    private func launch(role: CoreLinkRole, transport: LinkTransport, body: @escaping () throws -> Void) {
        guard worker == nil || worker?.isFinished == true else { return }
        flags.reset()
        publish { state in
            state = LinkState(role: role, transport: transport, step: .waitingForPeer)
        }
        let thread = Thread { [weak self] in
            do {
                try body()
            } catch {
                self?.log.warning("Link ceremony failed: \(String(describing: error), privacy: .public)")
                self?.publish { state in
                    state.step = .failed
                    state.failure = String(describing: error)
                }
            }
        }
        thread.name = "cruisemesh.link"
        thread.qualityOfService = .userInitiated
        worker = thread
        thread.start()
    }

    /// Every state change lands on the main queue, because `@Published` drives a
    /// SwiftUI view and everything below runs on the ceremony's own thread.
    private func publish(_ mutate: @escaping (inout LinkState) -> Void) {
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            var next = self.state
            mutate(&next)
            self.state = next
        }
    }

    // -----------------------------------------------------------------------
    // §9.1 + §9.3-§9.4(b): the device being adopted
    // -----------------------------------------------------------------------

    private func runNewDevice(transport: LinkTransport) throws {
        // Written out rather than as two ternaries: `try` may not appear to the
        // right of the conditional operator.
        var listener: LinkLanListener?
        var relayConfig: RelayConfig?
        switch transport {
        case .lan:
            listener = try LinkLanListener.open()
        case .relay:
            relayConfig = try requireRelay()
        }
        defer { listener?.close() }

        // §9.1 and DL-5: this device's own endpoints, and nothing it has merely
        // observed about anyone else's.
        let newDevice = try CoreLinkNewDevice(
            lanEndpoints: listener?.endpoints ?? [],
            relayBaseUrls: relayConfig.map { [$0.relayUrl] } ?? [],
            nowMs: now(),
            budgets: nil
        )
        let machine = NewDeviceMachine(newDevice)
        let wire: LinkWire
        switch transport {
        case .lan:
            wire = LinkLanAcceptingWire(listener: listener!)
        case .relay:
            wire = try LinkRelayWire(
                config: relayConfig!,
                rendezvousId: machine.rendezvousId(),
                sendLane: .toApprovingDevice,
                receiveLane: .toNewDevice,
                clock: linkNowMs,
                sleep: linkPause
            )
        }
        defer { wire.close() }

        let qrText = machine.qrText
        publish { $0.qrText = qrText }
        let summary = try drive(machine: machine, wire: wire)
        guard summary.outcome == .channelReady else { return }
        try adoptFromChannel(machine: machine, wire: wire, summary: summary)
    }

    /// §9.3 and §9.4 on the new device: go silent, name our keys, take the
    /// export, acknowledge the exact roster head, and only then become visible.
    private func adoptFromChannel(
        machine: LinkMachine,
        wire: LinkWire,
        summary: CoreLinkSummary
    ) throws {
        guard let binding = summary.channelBinding else {
            throw LinkSessionError.missingChannelBinding
        }
        let device = DeviceKeyStore.loadOrCreate()

        // §9.4: the silence starts here, before the export is on the wire — not
        // once it has landed. Everything from this line until the
        // acknowledgement is a device the mesh cannot hear. The binding goes in
        // with it, so the export that arrives has to be THIS ceremony's.
        _ = try store.beginLinkActivation(channelBinding: binding, nowMs: now())
        // From this line the window is open, and every path out of it that is not
        // `completeLinkActivation` has to close it again — or a ceremony that
        // failed leaves this phone permanently silent with no way back except
        // reinstalling the app.
        do {
            // And the radios go down with it. Core refuses everything it holds
            // from this moment; `LinkVisibility` is how the part core cannot see
            // — this phone's own BLE advertiser and Bonjour registration — hears
            // about it. The change hops the mesh controller's own queue, so this
            // waits for it to have actually happened rather than assuming it: a
            // frame sent in that gap goes out from a phone that is still
            // advertising, which is the one thing §9.4 forbids.
            LinkVisibility.refresh(store: store)
            guard LinkVisibility.awaitApplied(false, timeoutMs: Self.visibilityApplyTimeoutMs) else {
                throw LinkSessionError.didNotGoQuiet
            }

            publish { $0.step = .carryingBootstrap }
            try wire.send(machine.seal(
                coreLinkDeviceOffer(
                    deviceSignSk: device.signSk,
                    deviceAgreePk: device.agreePk,
                    channelBinding: binding
                )
            ))

            let bootstrap = try coreLinkBootstrapDecode(
                bytes: coreLinkBootstrapJoin(chunks: try collectBootstrap(machine: machine, wire: wire))
            )
            // The person id the restore flow read out of the `.cmbak`, when there
            // was one: core refuses a bootstrap from anybody else, so a person
            // who scanned the wrong phone's code is told before their store is
            // written to rather than afterwards. `nil` falls back to core's
            // factory-fresh rule alone, which is all a phone with no backup open
            // can check.
            let imported = try store.importLinkBootstrap(
                bootstrap: bootstrap,
                ownDeviceSignPk: device.signPk,
                expectedPersonId: expectedPersonId,
                nowMs: now()
            )

            publish { $0.step = .activating }
            // §9.4(b): the acknowledgement goes out first and the device becomes
            // visible second. The other ordering would make a device visible on
            // the strength of a message that never left.
            try wire.send(machine.seal(
                coreLinkActivationAck(
                    deviceSignSk: device.signSk,
                    rosterHead: imported.rosterHead,
                    channelBinding: binding
                )
            ))
            _ = try store.completeLinkActivation(ackedRosterHead: imported.rosterHead, nowMs: now())
            if let adopted = try? store.ownRoster() { rememberRosterSeen(adopted) }
            // Only now, on the far side of the acknowledgement: this phone is a
            // device of this person, so it takes their name and photo and stops
            // being a phone that has never been set up. A ceremony that failed
            // reaches `abandonActivation` below instead and leaves both untouched.
            LinkAdoption.adopted(profile: imported.profile)
            // Visible from here, radios included.
            LinkVisibility.refresh(store: store)

            // The two-phone smoke script's gate for §13 WP6 reads this line and
            // its twin on the approving device, and compares the heads. A head is
            // a hash of a public document and a device id is derived from a
            // public key, so neither is a secret — but no identity secret, QR
            // payload or SAS ever appears here, and none may be added.
            log.info(
                "link complete: role=newDevice deviceId=\(deviceIdHex(imported.ownDeviceId), privacy: .public) rosterHead=\(deviceIdHex(imported.rosterHead), privacy: .public) contacts=\(imported.contactsImported, privacy: .public) messages=\(imported.messagesImported, privacy: .public)"
            )

            let report = LinkReport(
                deviceIdHex: deviceIdHex(imported.ownDeviceId),
                rosterHeadHex: deviceIdHex(imported.rosterHead),
                rosterSeq: bootstrap.roster.seq,
                contacts: imported.contactsImported,
                groups: imported.groupsImported,
                messages: imported.messagesImported,
                catchUpChats: imported.catchUp.count
            )
            publish { state in
                state.step = .done
                state.report = report
            }
        } catch {
            abandonActivation()
            throw error
        }
    }

    /// Give §9.4's gates back after a ceremony that did not finish.
    ///
    /// Every failure inside the window comes through here: a declined confirm, a
    /// dropped socket, a bootstrap that never finished arriving, a person who
    /// tapped Stop, an import core refused. The core call is a no-op on a store
    /// that never opened a window and refuses outright on one that completed, so
    /// it is safe to call without first asking where the ceremony got to.
    private func abandonActivation() {
        do {
            _ = try store.abandonLinkActivation(nowMs: now())
        } catch {
            log.warning("Could not reopen the gates after a failed link: \(String(describing: error), privacy: .public)")
        }
        LinkVisibility.refresh(store: store)
    }

    /// Take sealed chunks until they make a whole export.
    ///
    /// The completeness test is the core's own reassembly, not a header this
    /// shell learned to read: `coreLinkBootstrapJoin` refuses a stream that is
    /// short, reordered, or duplicated, so "it joined" is exactly "it is all
    /// here" and the shell holds no opinion about the format.
    private func collectBootstrap(machine: LinkMachine, wire: LinkWire) throws -> [Data] {
        var chunks: [Data] = []
        let deadline = now() + Self.bootstrapDeadlineMs
        while now() < deadline {
            if flags.isCancelled { throw LinkSessionError.cancelled }
            guard let frame = try wire.receive(waitMs: Self.receiveWaitMs) else { continue }
            chunks.append(try machine.open(frame))
            if (try? coreLinkBootstrapJoin(chunks: chunks)) != nil { return chunks }
        }
        throw LinkSessionError.bootstrapNeverArrived
    }

    // -----------------------------------------------------------------------
    // §9.2 + §9.3-§9.4(a): the device doing the adopting
    // -----------------------------------------------------------------------

    private func runApprovingDevice(qrText: String, transport: LinkTransport) throws {
        let device = DeviceKeyStore.loadOrCreate()
        let roster = try ownRoster(device: device)
        let approver = try CoreLinkApprovingDevice.scan(
            qrText: qrText,
            activeDeviceCount: UInt32(roster.devices.count),
            budgets: nil
        )
        let machine = ApprovingDeviceMachine(approver)
        let wire: LinkWire
        switch transport {
        case .lan:
            wire = try LinkLanDialer.connect(
                endpoints: machine.rendezvous().lanEndpoints,
                connectTimeoutMs: Self.connectTimeoutMs
            )
        case .relay:
            wire = try LinkRelayWire(
                config: try requireRelay(),
                rendezvousId: machine.rendezvousId(),
                sendLane: .toNewDevice,
                receiveLane: .toApprovingDevice,
                clock: linkNowMs,
                sleep: linkPause
            )
        }
        defer { wire.close() }

        let summary = try drive(machine: machine, wire: wire)
        guard summary.outcome == .channelReady else { return }
        try adoptOverChannel(
            machine: machine,
            wire: wire,
            summary: summary,
            device: device,
            roster: roster
        )
    }

    /// §9.3 and §9.4(a): sign the roster at `seq + 1` and stream the export.
    private func adoptOverChannel(
        machine: LinkMachine,
        wire: LinkWire,
        summary: CoreLinkSummary,
        device: DeviceKeypair,
        roster: Roster
    ) throws {
        guard let binding = summary.channelBinding else {
            throw LinkSessionError.missingChannelBinding
        }

        publish { $0.step = .carryingBootstrap }
        let offer = try coreLinkOpenDeviceOffer(
            frame: machine.open(try awaitFrame(wire: wire)),
            channelBinding: binding
        )
        let update = try coreLinkSignNewDeviceRoster(
            current: roster,
            personRootSignPk: identity.signPk,
            approvingDeviceSignSk: device.signSk,
            newDeviceSignPk: offer.deviceSignPk,
            newDeviceAgreePk: offer.deviceAgreePk
        )

        // Everything older than the head is WP4's catch-up, not this ceremony's.
        // Signed with the roster-signing device's key and bound to this channel,
        // so the phone on the other end can tell this export from a file. 0 = the
        // core's own defaults for head size and lifetime.
        let bootstrap = try store.buildLinkBootstrap(
            identity: identity,
            // The person's own name and photo, so the phone being adopted never
            // has to ask them who they are (§9.3). Read here, on the device that
            // has the answer; written on the other side by `LinkAdoption`.
            profile: LinkAdoption.profileOf(),
            roster: update.roster,
            approvingDeviceSignSk: device.signSk,
            channelBinding: binding,
            historyHeadPerChat: 0,
            lifetimeMs: 0,
            nowMs: now()
        )
        let payload = try coreLinkBootstrapEncode(bootstrap: bootstrap)
        for chunk in try coreLinkBootstrapChunks(payload: payload) {
            try wire.send(machine.seal(chunk))
        }

        publish { $0.step = .activating }
        // The acknowledgement must come from the device that made the offer, not
        // merely from some device the roster lists: on a fleet that already has
        // siblings, any of them would otherwise satisfy the check and this side
        // would record a link the new phone never finished.
        let ack = try coreLinkOpenActivationAck(
            frame: machine.open(try awaitFrame(wire: wire)),
            roster: update.roster,
            offeredDeviceSignPk: offer.deviceSignPk,
            channelBinding: binding
        )
        // The new device has acknowledged the exact head, so this fleet is now
        // two devices and both of them know it.
        try store.adoptOwnRoster(
            roster: update.roster,
            personRootSignPk: identity.signPk,
            ownDeviceId: device.deviceId
        )
        rememberRosterSeen(update.roster)

        // §9.5: "roster gossips to contacts". Fired here, on the approving
        // device, because this is the line where a fleet larger than one device
        // becomes real — and fired ONLY here, not on the new device as well. The
        // newly adopted phone imported the contact list but not the ledger of who
        // has already been told, so an announcement from it would seal a second
        // identical copy of the same document to every contact. One telling is
        // the whole product of this call.
        //
        // Core makes that a rule rather than an arrangement between call sites:
        // `announceOwnRoster` returns the empty shape unless the identity it is
        // given is the person the roster is about, and a linked sibling signs its
        // mail with a per-device identity. So a sibling's routine passes — relay,
        // mesh start — are already silent, and this remains the only routine
        // announcer. A sibling that ever does hold the person identity is a
        // fallback, not a second voice: the ledger makes its copy a no-op for
        // every contact the approving device has already told.
        RosterGossipSender.announceIfOwed(store: store, identity: identity, nowMs: now())

        // The other half of the smoke script's converge gate: same head, both
        // phones, written down by each of them independently.
        log.info(
            "link complete: role=approvingDevice deviceId=\(deviceIdHex(ack.deviceId), privacy: .public) rosterHead=\(deviceIdHex(update.rosterHead), privacy: .public) devices=\(update.roster.devices.count, privacy: .public)"
        )

        let report = LinkReport(
            deviceIdHex: deviceIdHex(ack.deviceId),
            rosterHeadHex: deviceIdHex(update.rosterHead),
            rosterSeq: update.roster.seq,
            contacts: 0,
            groups: 0,
            messages: 0,
            catchUpChats: 0
        )
        publish { state in
            state.step = .done
            state.report = report
        }
    }

    /// This person's own roster, minting §3's genesis the first time.
    ///
    /// The genesis roster is root-signed, because at `seq == 0` there is nothing
    /// else yet to sign it — this is the identity upgrade §2 asks for, where the
    /// deployed Ed25519 key becomes the person root and the phone holding it
    /// becomes device one.
    private func ownRoster(device: DeviceKeypair) throws -> Roster {
        if let existing = try store.ownRoster() { return existing }
        let genesis = try coreLinkGenesisRoster(
            personRootSignSk: identity.signSk,
            deviceSignPk: device.signPk,
            deviceAgreePk: device.agreePk
        )
        try store.adoptOwnRoster(
            roster: genesis,
            personRootSignPk: identity.signPk,
            ownDeviceId: device.deviceId
        )
        rememberRosterSeen(genesis)
        return genesis
    }

    // -----------------------------------------------------------------------
    // Shared plumbing
    // -----------------------------------------------------------------------

    /// Whether this install can finish §9.5 at all: it holds the roster-signing
    /// role, or there is no roster yet and it is about to mint §3's genesis and
    /// become device one.
    ///
    /// The backstop behind "Your devices"' gate on the same rule. Both exist
    /// because the signature is the LAST step of the ceremony: without this, a
    /// sibling could be walked through a code, a camera and six digits and fail
    /// after the person had done everything right. Fails closed — a store that
    /// cannot answer is not one to start a ceremony on the strength of.
    func canSignRoster() -> Bool {
        do {
            // No roster at all is a yes: this install is about to mint §3's
            // genesis and become device one. A store that *throws* is a no.
            guard let roster = try store.ownRoster() else { return true }
            guard let ownDeviceId = DeviceKeyStore.load()?.deviceId else { return false }
            return roster.approvingDeviceId == ownDeviceId
        } catch {
            return false
        }
    }

    /// Stamp this phone's private "seen here since" note for every device in a
    /// roster it has just adopted. At adoption, never at render — see
    /// `DeviceNameStore.rememberRoster`.
    private func rememberRosterSeen(_ roster: Roster) {
        DeviceNameStore.rememberRoster(
            deviceIdHexes: coreRosterDeviceIds(roster: roster).map(deviceIdHex),
            nowMs: now()
        )
    }

    private func drive(machine: LinkMachine, wire: LinkWire) throws -> CoreLinkSummary {
        let driver = LinkCeremonyDriver(
            machine: machine,
            wire: wire,
            clock: linkNowMs,
            sleep: linkPause,
            observer: LinkCeremonyObserver(
                onQr: { [weak self] qrText in self?.publish { $0.qrText = qrText } },
                onSas: { [weak self] sas, confirmHere, warnSoftCap in
                    self?.publish { state in
                        state.step = .comparingDigits
                        state.sas = sas
                        state.confirmHere = confirmHere
                        state.warnSoftCap = warnSoftCap
                    }
                },
                onProgress: { [weak self] sent in
                    guard sent else { return }
                    self?.publish { state in
                        if state.step == .waitingForPeer { state.step = .handshaking }
                    }
                }
            ),
            confirmDecision: { [weak self] in self?.flags.decision ?? .waiting },
            cancelled: { [weak self] in self?.flags.isCancelled ?? true }
        )
        let summary = try driver.run()
        publish { state in
            state.outcome = summary.outcome
            state.step = summary.outcome == .channelReady ? .carryingBootstrap : .failed
        }
        return summary
    }

    private func awaitFrame(wire: LinkWire) throws -> Data {
        let deadline = now() + Self.bootstrapDeadlineMs
        while now() < deadline {
            if flags.isCancelled { throw LinkSessionError.cancelled }
            if let frame = try wire.receive(waitMs: Self.receiveWaitMs) { return frame }
        }
        throw LinkSessionError.peerStoppedAnswering
    }

    private func requireRelay() throws -> RelayConfig {
        guard let config = RelayConfigStore.load() else { throw LinkSessionError.noRelayPass }
        return config
    }

    private func now() -> Int64 { linkNowMs() }
}

/// The ceremony's only clock, and its only sleep.
///
/// Free functions rather than methods so the driver and both wires can be handed
/// them without capturing a session that a cancelled run may already have let go
/// of — and so a test can drive the same loop against a fake clock by passing its
/// own pair.
func linkNowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1_000) }

func linkPause(_ ms: Int64) {
    guard ms > 0 else { return }
    Thread.sleep(forTimeInterval: Double(ms) / 1_000)
}

enum LinkSessionError: Error {
    case missingChannelBinding
    case didNotGoQuiet
    case cancelled
    case bootstrapNeverArrived
    case peerStoppedAnswering
    case noRelayPass
}

/// The two pieces of state the UI thread writes and the ceremony thread reads.
///
/// A small lock rather than `@Published` or an actor: the ceremony loop asks for
/// both of these many times a second from a plain `Thread`, and neither answer
/// may involve hopping a queue.
final class LinkSessionFlags {
    private let lock = NSLock()
    private var cancelled = false
    private var confirm: LinkConfirmDecision = .waiting

    var isCancelled: Bool {
        lock.lock(); defer { lock.unlock() }
        return cancelled
    }

    var decision: LinkConfirmDecision {
        lock.lock(); defer { lock.unlock() }
        return confirm
    }

    func reset() {
        lock.lock(); defer { lock.unlock() }
        cancelled = false
        confirm = .waiting
    }

    func requestCancel() {
        lock.lock(); defer { lock.unlock() }
        cancelled = true
    }

    func setDecision(_ decision: LinkConfirmDecision) {
        lock.lock(); defer { lock.unlock() }
        confirm = decision
    }
}
