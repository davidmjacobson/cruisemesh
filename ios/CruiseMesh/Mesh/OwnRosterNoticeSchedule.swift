import Foundation

/// When a live own-device link is owed this person's roster again
/// (`specs/multi-device-v1.md` §10 step 5).
///
/// §10 step 5 shipped edge-triggered: the notice was built and pushed at the
/// instant a HELLO2 arrived on an own-device link, and at no other moment. A
/// removal that happened while such a link was **already up** therefore had no
/// carrier at all — no new HELLO, no new offer — and the removed phone went on
/// believing it was linked. In the field that lasted 26 minutes, survived a
/// force-stop of both apps, and survived a reboot.
///
/// The fix is to make it level-triggered, and this is the bookkeeping for that:
/// per link, the last time a notice was written to it, so the periodic LAN pass
/// can re-offer on the cadence core defines (`coreOwnRosterNoticeReofferDue`).
/// Deliberately a timer rather than a roster-changed event:
///
///  - the frame is idempotent in both directions (the sender rebuilds it from
///    the store; the receiver's core refuses anything that does not strictly
///    supersede what it holds), so a re-offer that says nothing new costs one
///    small signed document;
///  - a timer cannot be missed. An event can — by a process that was not
///    running when the roster changed, by a link that came up afterwards, by a
///    crash between the commit and the send. This is the mechanism that has to
///    work on the phone that is *wrong*, so it must not depend on anything
///    having been delivered to it.
///
/// Also holds the capability bits the link's HELLO2 carried, because §10 step
/// 5's other precondition is that the peer said it can read a notice at all,
/// and that fact only crosses the wire once per link.
///
/// Mirrors Android's `OwnRosterNoticeSchedule`. Callers hold `meshQueue`.
final class OwnRosterNoticeSchedule {
    private struct LinkState {
        let capabilities: UInt32
        var lastOfferedAtMs: Int64?
    }

    private var links: [String: LinkState] = [:]

    /// HELLO2 nudges already spent per own-device link. A link with no entry in
    /// `links` has never heard the peer's HELLO2, so it can never become
    /// eligible for a notice; see `claimHello2Nudge`.
    private var hello2Nudges: [String: Int] = [:]

    /// How many HELLO2 nudges one own-device link is worth.
    static let nudgeLimit = 6

    /// A HELLO2 landed on an own-device link, carrying what that phone can read.
    func noteHello2(address: String, capabilities: UInt32) {
        let lastOfferedAtMs = links[address]?.lastOfferedAtMs
        links[address] = LinkState(capabilities: capabilities, lastOfferedAtMs: lastOfferedAtMs)
        hello2Nudges.removeValue(forKey: address)
    }

    /// A notice actually reached the wire on `address`.
    ///
    /// Called only for a write the router accepted. A send that failed has told
    /// this link nothing, and booking it as delivered sits the link out another
    /// whole interval — on a half-open own-device link, exactly the state the
    /// heartbeat exists to catch.
    func noteOffered(address: String, nowMs: Int64) {
        guard var state = links[address] else { return }
        state.lastOfferedAtMs = nowMs
        links[address] = state
    }

    /// Whether this tick may re-send our HELLO2 to `address`, spending one of a
    /// small budget.
    ///
    /// The re-offer is level-triggered, but its precondition — what the peer
    /// says it can read — still crosses the wire exactly once per link, on a
    /// single frame at establishment. A HELLO2 lost to a reordering leaves the
    /// link permanently ineligible for a notice: the same "one delivered event"
    /// failure the level-trigger was added to remove. So the tick nudges.
    ///
    /// False once the peer's HELLO2 has arrived (nothing left to shake loose) or
    /// once the budget is spent (a peer that will not answer must not be sent a
    /// frame every tick for the life of the link).
    func claimHello2Nudge(address: String) -> Bool {
        guard links[address] == nil else { return false }
        let spent = hello2Nudges[address] ?? 0
        guard spent < Self.nudgeLimit else { return false }
        hello2Nudges[address] = spent + 1
        return true
    }

    /// The link closed; nothing is owed to it.
    func forget(address: String) {
        links.removeValue(forKey: address)
        hello2Nudges.removeValue(forKey: address)
    }

    func clear() {
        links.removeAll()
        hello2Nudges.removeAll()
    }

    /// The capability bits to re-offer with, or nil when this link is not due
    /// one right now (or never said it could read one).
    func dueCapabilities(address: String, nowMs: Int64) -> UInt32? {
        guard let state = links[address] else { return nil }
        guard OwnRosterNoticePolicy.peerReadsNotices(peerCapabilities: state.capabilities) else {
            return nil
        }
        guard coreOwnRosterNoticeReofferDue(
            lastOfferedAtMs: state.lastOfferedAtMs,
            nowMs: nowMs
        ) else { return nil }
        return state.capabilities
    }
}
