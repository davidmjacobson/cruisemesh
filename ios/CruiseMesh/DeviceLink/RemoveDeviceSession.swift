import Foundation
import os.log

/// What a removal attempt ended as, in facts rather than copy.
enum RemoveDeviceResult: Equatable {
    /// The device is buried, the fleet's inbox key is rotated, and contacts have
    /// been told (or the telling is queued).
    ///
    /// `relayRotationQueued` says §10.2 is owed: this person has a Shore Pass,
    /// so the shared relay credential is queued to be re-keyed on the next pass
    /// that reaches the relay. False on the installs with no pass to rotate.
    case removed(
        contactsTold: Int,
        siblingsToHandOffTo: Int,
        unresealableRecords: UInt32,
        relayRotationQueued: Bool
    )

    /// Nothing was changed. The reason is for the shell to word.
    case refused(RemoveDeviceRefusal)
}

/// Why a removal could not be attempted, or did not finish.
enum RemoveDeviceRefusal: Equatable {
    /// No roster: this person has never linked a second device.
    case noDevices

    /// §10.1: only the device holding the roster-signing role can sign this.
    case notTheApprovingDevice

    /// This device does not hold the inbox key the roster names, so it cannot
    /// rotate it — a sibling that has not caught up with an earlier revocation.
    /// Refusing is the whole point: rotating from a key this device cannot open
    /// would strand the retained backlog.
    case inboxKeyMissing

    /// This install has no device keys, so it is not part of any roster.
    case noDeviceKeys

    /// An earlier removal was written down, its key reached storage, and it never
    /// committed — so this device may have re-sealed part of its backlog to that
    /// key already. Planning a fresh removal would mint different material at the
    /// same generation and leave those rows unreadable forever, which is §10's one
    /// forbidden outcome. See `RemoveDeviceSession.repairPending`.
    case earlierRemovalUnfinished

    /// Core refused the update or the commit. Details are in the log.
    case coreRefused
}

/// §10.1's "Remove device", end to end, on the device that may sign it.
///
/// The order below is not this type's invention — `commit_own_revocation`
/// documents it and refuses to run out of order. What this class owes is the one
/// step core cannot take: making the rotated inbox key durable *between* the two
/// calls, so that a crash mid-ceremony leaves a fleet that can finish rather than
/// a backlog sealed to a secret that only ever existed in memory.
///
/// 1. Ask core for the update (`coreRevokeDevicesRoster`) — the tombstone, the
///    `seq + 1`, the re-signed certificates and the freshly minted key.
/// 2. `beginOwnRevocation` writes the journal row and hands over the key.
/// 3. `InboxKeyStore.save` makes it durable. **This is the load-bearing step.**
/// 4. `commitOwnRevocation` re-seals the backlog, adopts the roster, and points
///    the inbound gate at it.
/// 5. `RosterGossipSender` tells the contacts — §10.1 step 4's surface for them
///    is fed by the roster they now hold, not by a separate notice.
///
/// 6. `RelayRotationDriver.begin` writes §10.2's rotation journal — and stops
///    there. The relay call itself belongs to the relay pass, which is the only
///    place that knows which network to reach the relay on and how fast it may
///    speak; see that type for why the removal must not wait on it.
///
/// # What it deliberately does not do
///
/// It does not make a network call. A removal that could fail on connectivity
/// would be one a person has to retry by guessing, and "my phone was stolen" is
/// not a moment to hand somebody a network error. So the relay `family_token`
/// rotation is *planned and written down* here and performed by the next relay
/// pass that can reach the relay — seconds later on a connected phone, whenever
/// the ship finds internet otherwise. Until it lands the removed device still
/// holds a working relay credential; the confirm copy says so rather than
/// promising an instant it cannot deliver.
///
/// It also does not deliver `RevocationCommit.handoffs` to siblings. Those ride
/// self-sync, which has no shell transport yet either; `revocationHandoffsFor`
/// exists precisely so a sibling can be handed the rotation whenever it is next
/// reachable. The count is returned so the surface can be honest that other
/// devices catch up when they are next online.
///
/// Mirrors Android's `RemoveDeviceSession.kt`.
struct RemoveDeviceSession {
    private let identity: Identity
    private let store: MessageStore
    private let log = Logger(subsystem: "com.cruisemesh", category: "RemoveDevice")

    init(identity: Identity, store: MessageStore = AppStore.get()) {
        self.identity = identity
        self.store = store
    }

    /// Settle whatever an interrupted ceremony left behind, before planning a new
    /// one.
    ///
    /// Core states the whole decision procedure and this is it, unembellished: a
    /// device that wakes to a pending revocation asks its own key store whether it
    /// holds that generation. *No* means nothing was ever re-sealed to it, so the
    /// journal row is worth nothing and is dropped. *Yes* means the backlog may
    /// already be addressed to that key, and a fresh plan would mint different
    /// material at the same generation — so this refuses rather than guessing.
    ///
    /// Returns nil when there is nothing in the way.
    private func repairPending() -> RemoveDeviceRefusal? {
        let pending: PendingRevocation?
        do {
            pending = try store.pendingOwnRevocation()
        } catch {
            log.warning("Could not read the unfinished removal: \(String(describing: error), privacy: .public)")
            return .coreRefused
        }
        guard let pending else { return nil }

        if InboxKeyStore.generation() == pending.inboxKeyGeneration {
            return .earlierRemovalUnfinished
        }
        do {
            _ = try store.abandonOwnRevocation()
            return nil
        } catch {
            log.warning("Could not give up on an unfinished removal: \(String(describing: error), privacy: .public)")
            return .coreRefused
        }
    }

    func remove(
        deviceId: Data,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
    ) -> RemoveDeviceResult {
        if let blocked = repairPending() { return .refused(blocked) }

        let roster: Roster?
        do {
            roster = try store.ownRoster()
        } catch {
            log.warning("Could not read this person's device list: \(String(describing: error), privacy: .public)")
            return .refused(.coreRefused)
        }
        guard let roster else { return .refused(.noDevices) }

        guard let device = DeviceKeyStore.load() else { return .refused(.noDeviceKeys) }
        guard device.deviceId == roster.approvingDeviceId else {
            return .refused(.notTheApprovingDevice)
        }
        guard let currentInboxKey = InboxKeyStore.current(
            identity: identity,
            inboxKeyGeneration: roster.inboxKeyGeneration
        ) else {
            return .refused(.inboxKeyMissing)
        }

        do {
            let update = try coreRevokeDevicesRoster(
                current: roster,
                personRootSignPk: identity.signPk,
                approvingDeviceSignSk: device.signSk,
                revokedDeviceIds: [deviceId],
                currentInboxKey: currentInboxKey
            )
            // (2) and (3): the key is durable before anything is re-sealed to it.
            let rotated = try store.beginOwnRevocation(
                update: update,
                personRootSignPk: identity.signPk,
                ownDevice: device,
                nowMs: nowMs
            )
            InboxKeyStore.save(rotated)

            let commit = try store.commitOwnRevocation(
                update: update,
                personRootSignPk: identity.signPk,
                ownDevice: device,
                supersededInboxKey: currentInboxKey,
                nowMs: nowMs
            )
            DeviceNameStore.forget(deviceIdHex: deviceIdHex(deviceId))
            // §10.1 step 1's contact leg. Re-derived from the store rather than
            // read off `commit.contactUserIds`, so a commit that landed and then
            // crashed before sending is repaired by the next pass instead of
            // leaving contacts silently un-told.
            let told = RosterGossipSender.announceIfOwed(
                store: store,
                identity: identity,
                nowMs: nowMs
            )
            // §10.2, written down but not performed. The nudge is what makes
            // the common case feel instant: on a phone that is online the next
            // pass starts a moment later and the rotation lands with it.
            let rotationQueued = RelayRotationDriver(store: store).begin(revocation: commit)
            if rotationQueued { RelaySyncEvents.requestSync() }
            return .removed(
                contactsTold: told,
                siblingsToHandOffTo: commit.handoffs.count,
                unresealableRecords: commit.unresealableRecords,
                relayRotationQueued: rotationQueued
            )
        } catch {
            log.warning("Removing a device did not finish: \(String(describing: error), privacy: .public)")
            return .refused(.coreRefused)
        }
    }
}
