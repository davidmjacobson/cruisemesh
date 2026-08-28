package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.coreOwnRosterNoticeReofferDue

/**
 * When a live own-device link is owed this person's roster again
 * (`specs/multi-device-v1.md` §10 step 5).
 *
 * §10 step 5 shipped edge-triggered: the notice was built and pushed at the
 * instant a HELLO2 arrived on an own-device link, and at no other moment. A
 * removal that happened while such a link was **already up** therefore had no
 * carrier at all — no new HELLO, no new offer — and the removed phone went on
 * believing it was linked. In the field that lasted 26 minutes, survived a
 * force-stop of both apps, and survived a reboot.
 *
 * The fix is to make it level-triggered, and this is the bookkeeping for that:
 * per link, the last time a notice was written to it, so the periodic LAN pass
 * can re-offer on the cadence core defines
 * ([coreOwnRosterNoticeReofferDue]). Deliberately a timer rather than a
 * roster-changed event:
 *
 *  - the frame is idempotent in both directions (the sender rebuilds it from
 *    the store; the receiver's core refuses anything that does not strictly
 *    supersede what it holds), so a re-offer that says nothing new costs one
 *    small signed document;
 *  - a timer cannot be missed. An event can — by a process that was not running
 *    when the roster changed, by a link that came up afterwards, by a crash
 *    between the commit and the send. This is the mechanism that has to work on
 *    the phone that is *wrong*, so it must not depend on anything having been
 *    delivered to it.
 *
 * Also holds the capability bits the link's HELLO2 carried, because §10 step
 * 5's other precondition is that the peer said it can read a notice at all, and
 * that fact only crosses the wire once per link.
 *
 * Methods are @Synchronized leaf-monitor style: a notice is written from a LAN
 * reader thread and re-offered from the periodic mesh tick.
 */
internal class OwnRosterNoticeSchedule {

    private data class LinkState(val capabilities: UInt, val lastOfferedAtMs: Long?)

    private val links = mutableMapOf<String, LinkState>()

    /**
     * HELLO2 nudges already spent per own-device link. A link with no entry in
     * [links] has never heard the peer's HELLO2, so it can never become
     * eligible for a notice; see [claimHello2Nudge].
     */
    private val hello2Nudges = mutableMapOf<String, Int>()

    /** A HELLO2 landed on an own-device link, carrying what that phone can read. */
    @Synchronized
    fun noteHello2(address: String, capabilities: UInt) {
        val existing = links[address]
        links[address] = LinkState(capabilities, existing?.lastOfferedAtMs)
        hello2Nudges.remove(address)
    }

    /**
     * A notice actually reached the wire on [address].
     *
     * Called only for a write the router accepted. A send that failed has told
     * this link nothing, and booking it as delivered sits the link out another
     * whole interval — on a half-open own-device link, exactly the state the
     * heartbeat exists to catch.
     */
    @Synchronized
    fun noteOffered(address: String, nowMs: Long) {
        val existing = links[address] ?: return
        links[address] = existing.copy(lastOfferedAtMs = nowMs)
    }

    /**
     * Whether this tick may re-send our HELLO2 to [address], spending one of a
     * small budget.
     *
     * The re-offer is level-triggered, but its precondition — what the peer
     * says it can read — still crosses the wire exactly once per link, on a
     * single frame at establishment. A HELLO2 lost to a reordering, or one that
     * arrived before this process had loaded its identity, leaves the link
     * permanently ineligible for a notice: the same "one delivered event"
     * failure the level-trigger was added to remove. So the tick nudges.
     *
     * False once the peer's HELLO2 has arrived (nothing left to shake loose) or
     * once the budget is spent (a peer that will not answer must not be sent a
     * frame every tick for the life of the link).
     */
    @Synchronized
    fun claimHello2Nudge(address: String): Boolean {
        if (links.containsKey(address)) return false
        val spent = hello2Nudges[address] ?: 0
        if (spent >= NUDGE_LIMIT) return false
        hello2Nudges[address] = spent + 1
        return true
    }

    /** The link closed; nothing is owed to it. */
    @Synchronized
    fun forget(address: String) {
        links.remove(address)
        hello2Nudges.remove(address)
    }

    @Synchronized
    fun clear() {
        links.clear()
        hello2Nudges.clear()
    }

    /**
     * The capability bits to re-offer with, or null when this link is not due
     * one right now (or never said it could read one).
     */
    @Synchronized
    fun dueCapabilities(address: String, nowMs: Long): UInt? {
        val state = links[address] ?: return null
        if (!OwnRosterNoticePolicy.peerReadsNotices(state.capabilities)) return null
        if (!coreOwnRosterNoticeReofferDue(state.lastOfferedAtMs, nowMs)) return null
        return state.capabilities
    }

    companion object {
        /** How many HELLO2 nudges one own-device link is worth. */
        const val NUDGE_LIMIT = 6
    }
}
