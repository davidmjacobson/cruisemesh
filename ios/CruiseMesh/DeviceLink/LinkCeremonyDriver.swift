import Foundation

/// One side of the §9.1-§9.2 ceremony, behind the only shape a driver needs.
///
/// The core exports the two halves as two objects with two different sets of
/// methods — deliberately, because only one of them has a `confirm` at all (§9.2
/// puts the tap on the device that is already part of the person). This protocol
/// is how one loop can drive either without being handed the power to confirm on
/// the wrong phone: `confirmHere` exists on both, and on the new device it is
/// unreachable, because that side's action never says `confirmHere`.
///
/// Mirrors Android's `LinkCeremonyDriver.kt`.
protocol LinkMachine: AnyObject {
    var role: CoreLinkRole { get }

    func start(nowMs: Int64) -> CoreLinkAction
    func resumeSent(nowMs: Int64) -> CoreLinkAction
    func resumePeerBytes(nowMs: Int64, bytes: Data) -> CoreLinkAction
    func tick(nowMs: Int64) -> CoreLinkAction
    func cancel(nowMs: Int64) -> CoreLinkSummary
    func summary() -> CoreLinkSummary?

    /// §9.2's explicit action. Only ever called on the approving device.
    func confirmHere(nowMs: Int64, matches: Bool) throws -> CoreLinkAction

    /// §9.3's seam, once the channel is confirmed.
    func seal(_ plaintext: Data) throws -> Data
    func open(_ frame: Data) throws -> Data
}

/// Raised when a build reaches a branch §9.2 exists to prevent.
enum LinkMachineMisuse: Error {
    /// The new device was asked to confirm its own link.
    case newDeviceCannotConfirm
}

final class NewDeviceMachine: LinkMachine {
    let role: CoreLinkRole = .newDevice
    private let core: CoreLinkNewDevice

    init(_ core: CoreLinkNewDevice) { self.core = core }

    var qrText: String { core.qrText() }
    func rendezvousId() throws -> Data { try core.rendezvousId() }

    func start(nowMs: Int64) -> CoreLinkAction { core.start(nowMs: nowMs) }
    func resumeSent(nowMs: Int64) -> CoreLinkAction { core.resumeSent(nowMs: nowMs) }
    func resumePeerBytes(nowMs: Int64, bytes: Data) -> CoreLinkAction {
        core.resumePeerBytes(nowMs: nowMs, bytes: bytes)
    }
    func tick(nowMs: Int64) -> CoreLinkAction { core.tick(nowMs: nowMs) }
    func cancel(nowMs: Int64) -> CoreLinkSummary { core.cancel(nowMs: nowMs) }
    func summary() -> CoreLinkSummary? { core.summary() }

    func confirmHere(nowMs: Int64, matches: Bool) throws -> CoreLinkAction {
        // Not "returns false" and not "does nothing": a build that reached here
        // would be a build in which the new phone confirms its own link, which
        // is the one thing §9.2 exists to prevent. Fail loudly, in a unit test,
        // long before a person is holding two phones.
        throw LinkMachineMisuse.newDeviceCannotConfirm
    }

    func seal(_ plaintext: Data) throws -> Data { try core.sealChannelFrame(plaintext: plaintext) }
    func open(_ frame: Data) throws -> Data { try core.openChannelFrame(frame: frame) }
}

final class ApprovingDeviceMachine: LinkMachine {
    let role: CoreLinkRole = .approvingDevice
    private let core: CoreLinkApprovingDevice

    init(_ core: CoreLinkApprovingDevice) { self.core = core }

    func rendezvous() -> LinkRendezvous { core.rendezvous() }
    func rendezvousId() throws -> Data { try core.rendezvousId() }

    func start(nowMs: Int64) -> CoreLinkAction { core.start(nowMs: nowMs) }
    func resumeSent(nowMs: Int64) -> CoreLinkAction { core.resumeSent(nowMs: nowMs) }
    func resumePeerBytes(nowMs: Int64, bytes: Data) -> CoreLinkAction {
        core.resumePeerBytes(nowMs: nowMs, bytes: bytes)
    }
    func tick(nowMs: Int64) -> CoreLinkAction { core.tick(nowMs: nowMs) }
    func cancel(nowMs: Int64) -> CoreLinkSummary { core.cancel(nowMs: nowMs) }
    func summary() -> CoreLinkSummary? { core.summary() }

    func confirmHere(nowMs: Int64, matches: Bool) throws -> CoreLinkAction {
        matches ? core.confirm(nowMs: nowMs) : core.decline(nowMs: nowMs)
    }

    func seal(_ plaintext: Data) throws -> Data { try core.sealChannelFrame(plaintext: plaintext) }
    func open(_ frame: Data) throws -> Data { try core.openChannelFrame(frame: frame) }
}

/// What the person has said about the digits, or that they have not said it yet.
enum LinkConfirmDecision { case waiting, matches, doesNotMatch }

/// Everything the driver tells the outside world. Pure reporting: an observer
/// that threw would be a shell bug, not a ceremony failure, so the driver does
/// not defend against it.
struct LinkCeremonyObserver {
    /// The offer to put on screen (new device only).
    var onQr: (String) -> Void = { _ in }
    /// The digits both screens must show. `confirmHere` is §9.2's button.
    var onSas: (String, Bool, Bool) -> Void = { _, _, _ in }
    /// Called once per loop turn, so a screen can show that something is
    /// happening.
    var onProgress: (Bool) -> Void = { _ in }
}

/// The shell loop the core's driver-boundary design asks for, with no UIKit in
/// it (`specs/multi-device-v1.md` §9.2).
///
/// It answers exactly one outstanding action at a time, which is the contract the
/// ceremony objects declare: a resume that does not match the action they are
/// waiting on changes nothing and is counted as a stale resume. Every wait is
/// bounded, every ending is the core's own named outcome, and the human tap
/// enters through `confirmDecision` rather than blocking a thread inside a state
/// machine.
///
/// Nothing here is iOS-specific and nothing here does IO, so both halves can be
/// run against each other in a unit test over a pair of queues — which is what
/// `LinkCeremonyDriverTests` does, and what has to be true before two phones are
/// worth picking up (§13's WP3 gate).
final class LinkCeremonyDriver {
    private let machine: LinkMachine
    private let wire: LinkWire
    private let clock: () -> Int64
    private let sleep: (Int64) -> Void
    private let observer: LinkCeremonyObserver
    private let confirmDecision: () -> LinkConfirmDecision
    private let cancelled: () -> Bool

    init(
        machine: LinkMachine,
        wire: LinkWire,
        clock: @escaping () -> Int64,
        sleep: @escaping (Int64) -> Void,
        observer: LinkCeremonyObserver = LinkCeremonyObserver(),
        confirmDecision: @escaping () -> LinkConfirmDecision = { .waiting },
        cancelled: @escaping () -> Bool = { false }
    ) {
        self.machine = machine
        self.wire = wire
        self.clock = clock
        self.sleep = sleep
        self.observer = observer
        self.confirmDecision = confirmDecision
        self.cancelled = cancelled
    }

    /// How long to wait on the wire while an offer or the digits are on screen.
    /// The core suggests a poll interval for its own `awaitPeer`; these two
    /// actions carry none, because from the core's side they are "a human is
    /// looking at something".
    ///
    /// Two seconds rather than one because these are the *long* phases — an offer
    /// can sit on a screen for minutes — and on a relay rendezvous every wait is
    /// a request against a family's own relay budget.
    private static let qrPollMs: Int64 = 2_000

    /// How often to re-ask whether the person has answered the digits.
    private static let confirmPollMs: Int64 = 250

    /// Run until the ceremony ends, and return the ending it named.
    ///
    /// `.channelReady` means the channel is open and the wire is still the way to
    /// it — §9.3's bootstrap and §9.4's acknowledgement ride the same pipe,
    /// sealed by `LinkMachine.seal`.
    func run() throws -> CoreLinkSummary {
        var action = machine.start(nowMs: clock())
        while true {
            if cancelled() { return machine.cancel(nowMs: clock()) }
            switch action.kind {
            case .finished(let summary):
                return summary

            case .sendBytes(let bytes):
                try wire.send(bytes)
                observer.onProgress(true)
                action = machine.resumeSent(nowMs: clock())

            case .showQr(let qrText):
                observer.onQr(qrText)
                action = try awaitPeer(waitMs: Self.qrPollMs)

            case .awaitPeer(let waitMs):
                action = try awaitPeer(waitMs: waitMs)

            case .showSas(let sas, let confirmHere, let warnSoftCap):
                observer.onSas(sas, confirmHere, warnSoftCap)
                if confirmHere {
                    switch confirmDecision() {
                    case .waiting:
                        // Still comparing two screens. The clock keeps running —
                        // a person who walks away hits the ceremony's deadline,
                        // not a stuck thread.
                        sleep(Self.confirmPollMs)
                        action = machine.tick(nowMs: clock())
                    case .matches:
                        action = try machine.confirmHere(nowMs: clock(), matches: true)
                    case .doesNotMatch:
                        action = try machine.confirmHere(nowMs: clock(), matches: false)
                    }
                } else {
                    // §9.2: this side shows the digits and waits for the other
                    // one's sealed answer. There is no local override.
                    action = try awaitPeer(waitMs: Self.qrPollMs)
                }
            }
        }
    }

    /// Wait for the peer, then resume with what arrived — or tick, which is how
    /// the core is told time passed without letting the shell decide what that
    /// means.
    private func awaitPeer(waitMs: Int64) throws -> CoreLinkAction {
        let bounded = min(max(waitMs, 0), LinkWireLimits.maxReceiveWaitMs)
        let bytes = try wire.receive(waitMs: bounded)
        observer.onProgress(false)
        if let bytes {
            return machine.resumePeerBytes(nowMs: clock(), bytes: bytes)
        }
        return machine.tick(nowMs: clock())
    }
}
