import Foundation
import os.log

/// How often §10 step 2's rotate call may be made.
///
/// The relay pass runs about once a minute and offers a pending rotation to
/// every one of those passes. Without something in between, a relay that
/// answers 500 — or a family whose pass lapsed — would be asked to re-key sixty
/// times an hour against a bucket that holds ten, and the family would be
/// rate-limited out of the one call it makes precisely when a phone has been
/// stolen. This is that something.
///
/// The *lengths* are core's (`coreRelayRotationNextStep`), because both shells
/// run the same ceremony against the same server and the reasoning behind
/// fifteen minutes belongs next to the server's bucket size. All this holds is
/// the two facts that are per-process: when the next attempt is allowed, and
/// how many have failed in a row.
///
/// It does not persist, deliberately. A relaunch forgets the ladder and allows
/// one attempt immediately, which is the behaviour worth having: the common
/// reason a rotation has not landed is that the phone was offline, and one call
/// per launch is a bound the relay's bucket is comfortable with. Mirrors
/// Android `RelayRotationPacer`.
final class RelayRotationPacer: @unchecked Sendable {
    private let lock = NSLock()
    private var notBeforeMs: Int64 = 0
    private var failures: Int = 0

    /// Consecutive failed attempts: how far up core's ladder we are.
    var consecutiveFailures: Int {
        lock.lock()
        defer { lock.unlock() }
        return failures
    }

    /// When the next attempt becomes allowed. `0` means "right now".
    var nextAttemptAtMs: Int64 {
        lock.lock()
        defer { lock.unlock() }
        return notBeforeMs
    }

    func mayAttempt(nowMs: Int64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return nowMs >= notBeforeMs
    }

    /// Record an attempt that did not end in a committed rotation, and hold the
    /// next one off for `delayMs`.
    ///
    /// The wait is a floor and never a ceiling: a later, shorter delay cannot
    /// pull an existing quiet window in, for the same reason the relay pass's
    /// own rate-limit window is a floor.
    func onFailure(nowMs: Int64, delayMs: Int64) {
        lock.lock()
        defer { lock.unlock() }
        failures += 1
        notBeforeMs = max(notBeforeMs, nowMs + max(delayMs, 0))
    }

    /// The rotation landed (or there was nothing to do): start clean.
    func onSettled() {
        lock.lock()
        defer { lock.unlock() }
        failures = 0
        notBeforeMs = 0
    }
}

/// Where this device keeps the family relay credential a rotation replaces.
protocol RelayRotationCredential {
    func current() -> RelayConfig?

    /// T23 epoch of the current endpoint; a rotation's epoch climbs above it.
    func epoch() -> Int64

    /// Write the rotated endpoint down as this device's own configuration —
    /// exactly what scanning a setup card does, which is why it goes through
    /// the same store and picks up the same epoch bump.
    func adopt(_ config: RelayConfig)
}

/// `RelayRotationCredential` over this install's saved Shore Pass.
struct SavedRelayCredential: RelayRotationCredential {
    func current() -> RelayConfig? { RelayConfigStore.load() }

    func epoch() -> Int64 { RelayConfigStore.relayEpoch() }

    func adopt(_ config: RelayConfig) {
        RelayConfigStore.save(relayUrl: config.relayUrl, relayToken: config.relayToken)
    }
}

/// What one turn of the driver did. Facts, for the log and for tests.
enum RelayRotationOutcome: Equatable {
    /// No rotation is owed.
    case nothingPending

    /// One is owed, but not yet — the associated value is when it may be tried.
    case waiting(notBeforeMs: Int64)

    /// The family's relay credential is now the replacement, here and on the
    /// server. `alreadyDone` is a retry that found the work already committed —
    /// a success, not a failure.
    case rotated(envelopesMoved: UInt64, alreadyDone: Bool)

    /// Not this time; the journal row survives and a later pass tries again.
    case deferred(RelayRotationNextStep)

    /// No retry could ever make this rotation happen; the journal was cleared.
    case gaveUp(RelayRotationNextStep)
}

/// **§10 step 2's driver**: the shell half of rotating the family's shared relay
/// `family_token` after a device is removed.
///
/// Core owns everything that can be got wrong quietly — minting the replacement
/// (`coreMintRelayMemberToken`), signing the request under the *person root*
/// (`relayEncodeRotateRequest`), refusing an answer about a different family
/// (`relayDecodeRotateResponse`), the crash-safety journal, and what a failed
/// call means (`coreRelayRotationNextStep`). What is left here is the part core
/// cannot do: making the HTTP call, and choosing when.
///
/// ## The ceremony, and why it is split across two moments
///
/// `begin` runs on the removal itself, straight after §10.1 commits: it plans
/// the rotation and writes the journal row, and does not touch the network. A
/// removal that could fail on connectivity would be a removal a person has to
/// retry by guessing, and "my phone was stolen" is not a moment to hand somebody
/// a network error. `rotateIfPending` runs from the relay sync pass, which is
/// the only place that already knows how to talk to the family relay. So the
/// removal is instant and the rotation lands on the first pass that can reach
/// the relay, which on a connected phone is the one the removal nudges into
/// running a second later.
///
/// ## Two credentials, one journal row
///
/// A rotation whose answer was lost leaves the server holding the replacement
/// and this device holding the question. That is why the first ask presents the
/// retired credential and a `401` is not treated as a failure: the only two
/// things that produce it are "the rotation landed and I did not hear" and "this
/// family is gone", and asking again under the replacement tells them apart.
/// relayd answers a repeat presentation with `rotated: false` and the same
/// values, so the second ask converges rather than rotating twice.
///
/// ## What it will not do
///
/// It never commits a rotation the relay did not confirm, it never lets the
/// *possession* of a token authorize anything (the signature is over both
/// credentials, under the person root, and relayd pins that key on first use),
/// and it never asks faster than `RelayRotationPacer` allows. That last one is
/// not fussiness: a rerun loop that ignored `Retry-After` once cost a family
/// ~290 posts a minute of its own allowance, and this route is the one a family
/// needs working on the day a phone is stolen.
///
/// Mirrors Android `RelayRotationDriver.kt`.
struct RelayRotationDriver {
    /// Process-wide, because the thing being paced is this device's calls to
    /// one relay and the driver is built fresh on every pass.
    static let sharedPacer = RelayRotationPacer()

    /// A call that reached the relay and produced nothing this device can use:
    /// a request core would not sign, or an answer that does not describe the
    /// rotation that was asked for. Paced like any other answered failure,
    /// because the relay's rotation bucket was charged for it either way, and
    /// never committed.
    private static let unusableAnswerStatus: UInt16 = 502

    private let log = Logger(subsystem: "com.cruisemesh", category: "RelayRotation")

    let store: MessageStore
    let credential: RelayRotationCredential
    /// The HTTP call: bearer + signed body in, response body out.
    let rotate: (RelayConfig, Data) throws -> Data
    /// Nudge a relay pass, because a rotation leaves work for one: the T23
    /// epoch bump this device just made has to reach every contact.
    ///
    /// Deliberately a nudge rather than the fan-out itself. Adopting the new
    /// endpoint is exactly what scanning a setup card does, and the machinery
    /// that notices *that* already clears the carried-upload markers, clears
    /// the group fan-out markers, queues the `CAP_RELAY_UPDATE` notices and
    /// re-scopes the friend directory — four things, in one place, driven off
    /// one epoch comparison. A second announcer here would race that comparison
    /// and silently take the marker clearing away from it.
    let onRotated: () -> Void
    let pacer: RelayRotationPacer
    let clock: () -> Int64

    init(
        store: MessageStore = AppStore.get(),
        credential: RelayRotationCredential = SavedRelayCredential(),
        rotate: @escaping (RelayConfig, Data) throws -> Data = RelayClient.rotateFamilyToken,
        onRotated: @escaping () -> Void = RelaySyncEvents.requestSync,
        pacer: RelayRotationPacer = RelayRotationDriver.sharedPacer,
        clock: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1_000) }
    ) {
        self.store = store
        self.credential = credential
        self.rotate = rotate
        self.onRotated = onRotated
        self.pacer = pacer
        self.clock = clock
    }

    /// Plan the rotation a revocation just earned and write it down. No network.
    ///
    /// Returns whether a rotation is now owed — false means this person has no
    /// Shore Pass to rotate, which is most installs.
    @discardableResult
    func begin(revocation: RevocationCommit) -> Bool {
        let now = clock()
        let alreadyPending: RelayRotationPlan?
        do {
            alreadyPending = try store.pendingRelayRotation()
        } catch {
            log.warning("Could not read the pending relay rotation: \(String(describing: error), privacy: .public)")
            return false
        }
        if alreadyPending != nil {
            // Deliberately not replaced. A pending row may name a credential
            // the server has already moved to, and overwriting it would throw
            // away the only record of that token -- locking the family out of
            // its own mailbox to lock one thief out. The rotation in flight
            // retires the same shared token this removal wants retired, so
            // finishing it is finishing this one too.
            log.info("A relay rotation is already pending; letting it finish rather than re-minting")
            return true
        }
        guard let config = credential.current() else { return false }
        let plan: RelayRotationPlan?
        do {
            plan = try corePlanRelayRotation(
                revocation: revocation,
                relayUrl: config.relayUrl,
                currentToken: config.relayToken,
                previousRelayEpoch: credential.epoch(),
                nowMs: now
            )
        } catch {
            // A deposit-class credential is the only way here: this device
            // cannot fetch its own mail either, so it is misconfigured and
            // rotating is not the repair.
            log.warning("This device's relay credential cannot be rotated: \(String(describing: error), privacy: .public)")
            return false
        }
        guard let plan else { return false }
        do {
            try store.beginRelayRotation(plan: plan, nowMs: now)
            // A fresh ceremony starts at the bottom of the ladder: whatever an
            // earlier rotation's failures earned is not this one's to serve.
            pacer.onSettled()
            log.info("Relay rotation planned; it lands on the next pass that reaches the relay")
            return true
        } catch {
            log.warning("Could not write the relay rotation down: \(String(describing: error), privacy: .public)")
            return false
        }
    }

    /// **§10.2's own-device leg, receiving side.** Pick up a replacement
    /// credential a sibling announced.
    ///
    /// The announcement rides §8's Settings stream, sealed to the fleet's inbox
    /// key, and core has already refused an inadmissible entry on the way in (an
    /// impossible epoch, an author this roster has buried). All that is left is
    /// to write the winner down.
    ///
    /// **It cannot fire yet, and that is the honest state of this leg**: no
    /// shell has a transport for sync records, so nothing but this device's own
    /// rotation ever writes that setting, and on this device the setting and the
    /// saved pass already agree. It is here rather than owed because the moment
    /// WP4's carrier lands, a sibling that slept through a removal repairs
    /// itself on its next relay pass with no further change — and because
    /// leaving the receiving half unwritten is how a leg ships half-done twice.
    ///
    /// The guard is deliberately narrow: same relay host, different token, and a
    /// pass configured on this device already. A phone whose person deliberately
    /// removed its Shore Pass must not have one reinstalled by a fleet
    /// announcement, and a phone on a different relay is not the family's to
    /// move.
    func adoptAnnouncedCredential() {
        let announced: RelayEndpoint?
        do {
            announced = try store.relayCredentialSetting()
        } catch {
            log.warning("Could not read the announced relay credential: \(String(describing: error), privacy: .public)")
            return
        }
        guard let announced, let saved = credential.current() else { return }
        guard saved.relayUrl == announced.url, saved.relayToken != announced.token else { return }
        log.info("Adopting the family relay credential a sibling announced")
        adopt(announced)
    }

    /// Finish whatever `begin` left owed, if the pacer allows an attempt now.
    @discardableResult
    func rotateIfPending(identity: Identity) -> RelayRotationOutcome {
        let pending: RelayRotationPlan?
        do {
            pending = try store.pendingRelayRotation()
        } catch {
            log.warning("Could not read the pending relay rotation: \(String(describing: error), privacy: .public)")
            return .nothingPending
        }
        guard let plan = pending else {
            pacer.onSettled()
            return .nothingPending
        }
        let now = clock()
        guard pacer.mayAttempt(nowMs: now) else {
            return .waiting(notBeforeMs: pacer.nextAttemptAtMs)
        }

        // The retired credential first. Its rejection is evidence, not failure.
        var answer = ask(plan: plan, identity: identity, bearer: plan.supersededToken, confirming: false)
        if case .refused(let step) = answer, step == .confirm {
            answer = ask(plan: plan, identity: identity, bearer: plan.newToken, confirming: true)
        }
        switch answer {
        case .answered(let rotation):
            return commit(plan: plan, rotation: rotation)
        case .refused(let step):
            return settle(plan: plan, step: step)
        }
    }

    private enum Ask {
        case answered(CoreRelayRotation)
        case refused(RelayRotationNextStep)
    }

    private func ask(
        plan: RelayRotationPlan,
        identity: Identity,
        bearer: String,
        confirming: Bool
    ) -> Ask {
        do {
            // Core signs, over BOTH credentials, with the person root -- never
            // the device key, and never a bare bearer token. relayd registers
            // that key on a family's first rotation and pins it after.
            let body = try relayEncodeRotateRequest(
                currentToken: bearer,
                newToken: plan.newToken,
                personRootSignSk: identity.signSk
            )
            let response = try rotate(RelayConfig(relayUrl: plan.relayUrl, relayToken: bearer), body)
            return .answered(try relayDecodeRotateResponse(body: response, expectedToken: plan.newToken))
        } catch let error as RelayHTTPError {
            return .refused(
                coreRelayRotationNextStep(
                    httpStatus: UInt16(clamping: error.statusCode),
                    relayCode: error.relayCode,
                    retryAfterMs: Int64(relayRetryAfterMs(retryAfterHeader: error.retryAfter)),
                    confirming: confirming,
                    consecutiveFailures: UInt32(pacer.consecutiveFailures)
                )
            )
        } catch let error as URLError {
            // No answer at all, so nothing was charged to the family's rotation
            // bucket and this may be retried in seconds rather than minutes.
            log.info("Relay rotation call did not reach the relay: \(error.localizedDescription, privacy: .public)")
            return .refused(
                coreRelayRotationNextStep(
                    httpStatus: 0,
                    relayCode: nil,
                    retryAfterMs: 0,
                    confirming: confirming,
                    consecutiveFailures: UInt32(pacer.consecutiveFailures)
                )
            )
        } catch {
            log.warning("The relay's rotation answer was unusable: \(String(describing: error), privacy: .public)")
            return .refused(
                coreRelayRotationNextStep(
                    httpStatus: Self.unusableAnswerStatus,
                    relayCode: nil,
                    retryAfterMs: 0,
                    confirming: confirming,
                    consecutiveFailures: UInt32(pacer.consecutiveFailures)
                )
            )
        }
    }

    private func commit(plan: RelayRotationPlan, rotation: CoreRelayRotation) -> RelayRotationOutcome {
        let now = clock()
        let committed: RelayRotationCommit
        do {
            committed = try store.commitRelayRotation(plan: plan, nowMs: now)
        } catch {
            // The server has already re-keyed the family; only the sibling
            // announcement failed. Adopt anyway -- this device must not be
            // locked out of a mailbox it can still open -- and leave the
            // journal row for a later pass to finish publishing.
            log.error("The rotated credential could not be announced to this person's other devices: \(String(describing: error), privacy: .public)")
            adopt(RelayConfig(relayUrl: plan.relayUrl, relayToken: plan.newToken))
            let delay = retryDelayMs()
            pacer.onFailure(nowMs: now, delayMs: delay)
            return .deferred(.retry(delayMs: delay))
        }
        adopt(committed.endpoint)
        pacer.onSettled()
        log.info(
            """
            Family relay credential rotated (rotated=\(rotation.rotated, privacy: .public), \
            \(rotation.envelopesMoved, privacy: .public) envelope(s) carried across, \
            \(committed.contactUserIds.count, privacy: .public) contact(s) to tell)
            """
        )
        return .rotated(envelopesMoved: rotation.envelopesMoved, alreadyDone: !rotation.rotated)
    }

    /// Write the credential down, and ask for a pass to carry the consequences.
    ///
    /// Contacts are told by the shipped T23 path, unchanged: saving bumps this
    /// device's relay epoch, and the next pass fans the *deposit* attenuation
    /// of the new token out to every one of them. Core's
    /// `encodeRelayUpdateContent` attenuates unconditionally, so no leg of this
    /// can put a member token on a contact's phone.
    private func adopt(_ endpoint: RelayEndpoint) {
        adopt(RelayConfig(relayUrl: endpoint.url, relayToken: endpoint.token))
    }

    private func adopt(_ config: RelayConfig) {
        credential.adopt(config)
        onRotated()
    }

    private func settle(plan: RelayRotationPlan, step: RelayRotationNextStep) -> RelayRotationOutcome {
        let now = clock()
        switch step {
        case .retry(let delayMs):
            pacer.onFailure(nowMs: now, delayMs: delayMs)
            return .deferred(step)
        case .remint:
            // Astronomically unlikely (32 bytes of OS randomness collided with
            // a credential this relay already holds), but the answer is cheap
            // and retrying the same token converges on nothing.
            var reminted = plan
            reminted.newToken = coreMintRelayMemberToken()
            reminted.newDepositToken = relayDepositTokenFor(memberToken: reminted.newToken)
            do {
                try store.beginRelayRotation(plan: reminted, nowMs: now)
            } catch {
                log.warning("Could not re-mint the replacement credential: \(String(describing: error), privacy: .public)")
            }
            pacer.onFailure(nowMs: now, delayMs: retryDelayMs())
            return .deferred(step)
        case .confirm:
            // Only reachable if a confirming ask somehow answered "confirm"
            // again; core does not, but treating it as a wait keeps this from
            // becoming a loop if that ever changes.
            pacer.onFailure(nowMs: now, delayMs: retryDelayMs())
            return .deferred(step)
        case .serverManagedToken, .notTheAuthority:
            // Neither can ever succeed from this device, so the honest thing is
            // to stop asking. The device keeps the credential it has; the
            // removed phone keeps it too, and the repair is a new token from
            // whoever can issue one.
            log.error(
                """
                This relay will not let this device rotate the family token \
                (\(String(describing: step), privacy: .public)); the removed device keeps its \
                relay credential until the pass is replaced
                """
            )
            do {
                _ = try store.abandonRelayRotation()
            } catch {
                log.warning("Could not clear the relay rotation: \(String(describing: error), privacy: .public)")
            }
            pacer.onSettled()
            return .gaveUp(step)
        }
    }

    /// Core's ladder for an answered failure, asked for without an answer to read.
    private func retryDelayMs() -> Int64 {
        let step = coreRelayRotationNextStep(
            httpStatus: Self.unusableAnswerStatus,
            relayCode: nil,
            retryAfterMs: 0,
            confirming: true,
            consecutiveFailures: UInt32(pacer.consecutiveFailures)
        )
        if case .retry(let delayMs) = step { return delayMs }
        return 0
    }
}
