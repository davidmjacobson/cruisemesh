package com.cruisemesh.app.mesh

import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.ContactRelayUnreachable
import uniffi.cruisemesh_core.coreContactRelayUnreachableDelta
import uniffi.cruisemesh_core.coreContactRelayUnreachableEndpointUsable
import java.util.concurrent.ConcurrentHashMap

/**
 * Contacts whose friend-card relay endpoint has stopped answering, and the
 * current sync pass's provisional observations.
 *
 * The counterpart of the persisted rejection streaks in core
 * `contact_relay_health`, for the half of the failure that produces no HTTP
 * answer to classify. A revoked token replies 401; a *retired host* replies
 * nothing at all, and that transport failure never reached the rejection
 * streak, so the address was re-dialled on every pass indefinitely.
 *
 * The committed rest is persisted by core and restored at the start of every
 * pass. Keeping only this pass's provisional set here preserves the immediate
 * per-envelope breaker while ensuring an app restart cannot re-arm a dead
 * endpoint from zero.
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
     * Endpoints that gave no answer during the pass now running, before that
     * observation has been judged. Keyed like [rests] and holding the same
     * address hash, so both arms agree about what a moved card means.
     */
    private val silentThisPass = ConcurrentHashMap<String, String>()

    /** Replace the committed rests with the store's authoritative snapshot. */
    fun restore(states: Collection<ContactRelayUnreachable>) {
        rests.clear()
        for (state in states) {
            rests[UserIdHex.encode(state.userId)] = Rest(
                state.endpointKey,
                state.unreachableStreak,
                state.unreachableAtMs,
            )
        }
    }

    /** Forgets the previous pass's provisional observations. */
    fun beginPass() {
        silentThisPass.clear()
    }

    /**
     * Records that this endpoint gave no answer at all during the pass now
     * running — a retired host, dead DNS, a refused connection, a TLS
     * certificate that does not cover the name. Returns true the first time in
     * a pass, so the caller can log the transition rather than every envelope.
     *
     * Provisional by design: [commitPass] decides at the end of the pass
     * whether this device had any business believing it.
     */
    fun noteUnreachableThisPass(userIdKey: String, endpointKey: String): Boolean =
        silentThisPass.put(userIdKey, endpointKey) != endpointKey

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
     *
     * The [silentThisPass] arm covers the *inside* of one pass, which the rest
     * window alone cannot: a rest is only awarded by [commitPass] once the pass
     * is over, so without this arm the first failure taught the pass nothing
     * and every remaining queued envelope re-dialled the same dead address.
     * Observed in the field — a friend card naming a host whose certificate no
     * longer covered it drew 352 handshakes in 27 seconds while an
     * update-restart backlog drained.
     *
     * That arm is deliberately not a rest and touches no streak. Whether the
     * silence counts at all still belongs to [commitPass], where the core can
     * weigh it against proof that this device's own internet works — a phone in
     * a tunnel fails every endpoint at once and must write off nobody. All this
     * arm claims is that an address which failed to answer milliseconds ago
     * will not answer the next envelope either.
     */
    fun endpointAnswering(userIdKey: String, endpointKey: String, nowMs: Long): Boolean {
        if (silentThisPass[userIdKey] == endpointKey) return false
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

    /**
     * Turns this pass's provisional observations into streaks and clears them,
     * returning the endpoints that earned a rest so the caller can say so.
     *
     * [otherRelayAnswered] is passed straight through to [noteSilentPass] —
     * see its doc for why the shell must not answer that question itself.
     */
    fun commitPass(otherRelayAnswered: Boolean, nowMs: Long): List<Pair<String, Long>> {
        val rested = silentThisPass.mapNotNull { (key, endpointKey) ->
            noteSilentPass(key, endpointKey, otherRelayAnswered, nowMs)?.let { key to it }
        }
        silentThisPass.clear()
        return rested
    }

    /** The endpoint answered: whatever we thought about its silence is settled. */
    fun noteAnswered(userIdKey: String) {
        rests.remove(userIdKey)
        silentThisPass.remove(userIdKey)
    }
}
