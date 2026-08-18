package com.cruisemesh.app.relay

/**
 * How often a device is allowed to try §10 step 2's rotate call.
 *
 * The relay pass runs about once a minute, and a pending rotation is offered
 * to it on every one of those passes. Without something in between, a relay
 * that answers 500 — or a family whose pass lapsed — would be asked to re-key
 * sixty times an hour against a bucket that holds ten, and the family would be
 * rate-limited out of the one call it makes precisely when a phone has been
 * stolen. This is that something.
 *
 * The *lengths* are core's ([`uniffi.cruisemesh_core.coreRelayRotationNextStep`]),
 * because both shells run the same ceremony against the same server and the
 * reasoning behind fifteen minutes belongs next to the server's bucket size.
 * All this holds is the two facts that are per-process: when the next attempt
 * is allowed, and how many have failed in a row.
 *
 * ## What it deliberately does not do
 *
 * It does not persist. A process restart forgets the ladder and allows one
 * attempt immediately, which is the behaviour worth having: the common reason
 * a rotation has not landed is that the phone was offline, and the common
 * reason a phone restarts the mesh service is that something about its
 * connectivity changed. One call per launch is a bound the relay's own bucket
 * is comfortable with, and it is what stops a rotation that is *owed* from
 * sitting out an hour because of a failure that is no longer true.
 *
 * No Android imports, so the policy is a plain unit test.
 */
class RelayRotationPacer {

    private var notBeforeMs: Long = 0L
    private var failures: Int = 0

    /** Consecutive failed attempts, which is how far up core's ladder we are. */
    val consecutiveFailures: Int get() = failures

    /** When the next attempt becomes allowed. `0` means "right now". */
    val nextAttemptAtMs: Long get() = notBeforeMs

    fun mayAttempt(nowMs: Long): Boolean = nowMs >= notBeforeMs

    /**
     * Record an attempt that did not end in a committed rotation, and hold the
     * next one off for [delayMs] (whatever core decided this failure earns).
     *
     * The wait is a floor and never a ceiling: a later, shorter delay cannot
     * pull an existing quiet window in, for the same reason the relay pass's
     * own rate-limit window is a floor — a second failure inside a long wait
     * must not become permission to ask sooner.
     */
    fun onFailure(nowMs: Long, delayMs: Long) {
        failures += 1
        notBeforeMs = maxOf(notBeforeMs, nowMs + maxOf(delayMs, 0L))
    }

    /** The rotation landed (or there was nothing to do): start clean. */
    fun onSettled() {
        failures = 0
        notBeforeMs = 0L
    }
}
