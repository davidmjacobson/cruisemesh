import Foundation
import os.log

/// DL-3's send side on this shell: put the envelopes core authored on a wire.
///
/// The exact twin of `RelayUpdateSender`, and for the same reason — a fact about
/// this person that every contact has to learn, sealed pairwise, one copy each,
/// riding the ordinary four transports. Core decides *who is owed the document
/// and what it says* (`MessageStore.announceOwnRoster`); this file decides
/// nothing at all beyond which socket to try first.
///
/// ## Why the triggers all collapse to one call
///
/// `specs/multi-device-v1.md` names three moments a contact must be told: a link
/// completing (§9.5), a revocation (§10.1's contact leg), and a contact added
/// after the roster last changed. Core's ledger answers all three with the same
/// question — who has not been told this head? — so this shell fires the same
/// idempotent call at each of them plus every relay pass, and a trigger that gets
/// missed is repaired by the next one instead of lost. On the install that has
/// never linked a device, which is very nearly the whole fleet, the call reads
/// one row and returns nothing.
///
/// Mirrors Android's `RosterGossipSender.kt`.
enum RosterGossipSender {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "RosterGossip")

    /// Tell every contact who is owed this person's roster, and send what was
    /// authored. Returns how many envelopes were queued.
    ///
    /// Failure to *send* is not failure to tell: the envelopes are durably queued
    /// by the time they are returned, so an offline contact is reached by carry,
    /// digest or the relay later. Failure to *author* is core's to report, and is
    /// swallowed here rather than propagated, because every call site is a
    /// best-effort moment on a background pass.
    @discardableResult
    static func announceIfOwed(
        store: MessageStore,
        identity: Identity,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000),
        sendToUser: (Data, Data) -> Bool = { userId, frame in
            MeshRouter.sendToUserId(userId: userId, frame: frame)
        },
        requestSync: () -> Void = { RelaySyncEvents.requestSync() },
        recordSeen: (Data) -> Void = { GossipState.seenIds.record(msgId: $0) }
    ) -> Int {
        let announcement: RosterGossipAnnouncement
        do {
            announcement = try store.announceOwnRoster(identity: identity, nowMs: nowMs)
        } catch {
            log.warning("Could not work out who is owed this device list: \(String(describing: error), privacy: .public)")
            return 0
        }
        guard !announcement.envelopes.isEmpty else { return 0 }

        // One sync request for the whole fan-out, not one per contact: the relay
        // pass uploads whatever is queued when it runs, and asking N times only
        // means N wake-ups for one batch of work.
        requestSync()
        for authored in announcement.envelopes {
            recordSeen(authored.envelope.msgId)
            // The recipient is the chat this pairwise message was filed under —
            // core says so on `RosterGossipAnnouncement.envelopes`, which is what
            // keeps this loop from re-pairing the list against a contact list it
            // walked separately and getting the pairing wrong.
            let recipient = authored.message.chatId
            if !sendToUser(recipient, authored.frame) {
                log.info("Queued device list for \(UserIdHex.encode(recipient), privacy: .public); peer not currently connected")
            }
        }
        log.info(
            "Told \(announcement.envelopes.count, privacy: .public) contact(s) about this device list (\(announcement.alreadyCurrent, privacy: .public) already had it, \(announcement.skippedBlocked, privacy: .public) blocked)"
        )
        return announcement.envelopes.count
    }
}

/// DL-3's narrower receive rule: a gossiped device list has to be about the
/// person who sealed it.
///
/// A restatement, not a decision. Core applies exactly this in
/// `deliver_inbound_body`'s `KIND_ROSTER_GOSSIP` arm
/// (`core/src/session/mesh_receive.rs`) and states the reasoning there: the
/// signature chain already refuses a *forged* document, but it would happily
/// accept a *genuine* one about a third party, replayed by anyone else holding a
/// copy — and a stale roster is precisely the document that still vouches for a
/// device its person has since buried.
///
/// It lives here because this shell's per-kind delivery has not moved onto
/// `core_deliver_inbound` yet, so nothing on the iOS receive path passes through
/// core's arm. When that migration lands, this function and its one caller go
/// with it. It is a function rather than an inline comparison so the rule is
/// testable and so removing it is a search for one name. Android carries the
/// identical restatement, and the two must be deleted together.
func rosterGossipDescribesSender(rosterPersonId: Data, senderUserId: Data) -> Bool {
    !rosterPersonId.isEmpty && rosterPersonId == senderUserId
}
