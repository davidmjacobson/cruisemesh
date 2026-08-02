package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.coreContactRelayUnreachableDelta
import uniffi.cruisemesh_core.coreContactRelayUnreachableEndpointUsable
import java.util.concurrent.ConcurrentHashMap

/**
 * Contacts whose friend-card relay endpoint has stopped answering this
 * process, and for how many consecutive sync passes.
 *
 * The counterpart of the persisted rejection streaks in core
 * `contact_relay_health`, for the half of the failure that produces no HTTP
 * answer to classify. A revoked token replies 401; a *retired host* replies
 * nothing at all, and that transport failure never reached the rejection
 * streak, so the address was re-dialled on every pass indefinitely.
 *
 * In memory rather than in the store, on both shells, for two reasons. A host
 * that is down is usually down for minutes, so re-learning it after a restart
 * costs two passes and is cheaper than carrying a stale verdict across days.
 * And it keeps "not answering right now" out of the persisted stale-card set
 * the contact sheet reads, where the prompt is "ask them to share their card
 * again" — correct for a revoked token, wrong for a relay that is rebooting.
 *
 * A plain class with no Android imports so the state machine can be unit
 * tested directly; [RelaySyncEngine] holds one for its lifetime. Mirrors
 * `ContactRelaySilence` in ios/CruiseMesh/Relay/RelaySweepSession.swift.
 */
internal class ContactRelaySilence {

    /**
     * [endpointKey] is `relayCursorKey(url, token)` — a hash, never the
     * credential — for the endpoint that was silent, so a rest is tied to the
     * address that earned it rather than to the person.
     */
    private data class Rest(val endpointKey: String, val streak: Long, val restedAtMs: Long)

    private val rests = ConcurrentHashMap<String, Rest>()

    /**
     * Whether this contact's endpoint has answered recently enough to be worth
     * spending a request on. True below the core's streak, and true again once
     * the rest window is up so a recovered host is picked back up with nobody
     * touching the phone.
     *
     * Also true the moment the contact's endpoint *moves*, which is the same
     * rule core applies to the persisted rejection streak: a new friend card
     * or a T23 relay-update notice that changes the address gives it a clean
     * slate, because a host that has never been tried cannot have been silent.
     * Without this a contact who migrated to a working relay would keep being
     * skipped for the rest of the half-hour window. Re-importing a card that
     * re-states the *same* endpoint changes nothing, exactly as it does not
     * launder a rejection streak.
     */
    fun endpointAnswering(userIdKey: String, endpointKey: String, nowMs: Long): Boolean {
        val rest = rests[userIdKey] ?: return true
        if (rest.endpointKey != endpointKey) {
            rests.remove(userIdKey)
            return true
        }
        return coreContactRelayUnreachableEndpointUsable(rest.streak, rest.restedAtMs, nowMs)
    }

    /**
     * Records one whole pass in which this endpoint said nothing.
     *
     * [otherRelayAnswered] is passed through to the core rather than tested
     * here: without same-pass proof that a different relay answered this
     * device, the core's delta is 0 and nothing is recorded, because the
     * failure is then most likely our own connectivity — a phone in a tunnel
     * fails every endpoint at once. Returns the new streak, or null when the
     * observation was not counted.
     */
    fun noteSilentPass(
        userIdKey: String,
        endpointKey: String,
        otherRelayAnswered: Boolean,
        nowMs: Long,
    ): Long? {
        val delta = coreContactRelayUnreachableDelta(otherRelayAnswered)
        if (delta == 0L) return null
        // A rest recorded against a different address says nothing about this
        // one, so the streak restarts rather than resuming.
        val prior = rests[userIdKey]?.takeIf { it.endpointKey == endpointKey }?.streak ?: 0L
        val streak = prior + delta
        rests[userIdKey] = Rest(endpointKey, streak, nowMs)
        return streak
    }

    /** The endpoint answered: whatever we thought about its silence is settled. */
    fun noteAnswered(userIdKey: String) {
        rests.remove(userIdKey)
    }
}
