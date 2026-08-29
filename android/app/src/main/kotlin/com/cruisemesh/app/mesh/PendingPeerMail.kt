package com.cruisemesh.app.mesh

/**
 * "We tried to hand something to this contact and had nowhere to send it."
 *
 * [MeshRouter.sendToUserId] returns false when no live link maps to a user
 * id; the frame is not lost (it lives in the store and redelivers on the next
 * digest sync) but that contact is now someone this phone has a concrete
 * reason to reach. [CentralConnectAdmission] reads that as
 * [BlePeerStanding.CONTACT_WITH_MAIL] so a contact with queued mail outranks
 * an idle peer for a scarce BLE link slot.
 *
 * Entries expire after [ttlMs] rather than being purely event-cleared. That
 * is deliberate and learned: [RadioPowerPolicy] originally escalated the
 * radio on "the carry queue is non-empty", which in a family that actually
 * uses the app is *always* true, so the escalation latched on permanently and
 * the whole policy went inert. A send that failed hours ago is not evidence
 * about now; only a recent one is.
 *
 * Plain class, no Android imports, unit-tested directly.
 */
class PendingPeerMail(private val ttlMs: Long = DEFAULT_TTL_MS) {
    private val waitingSinceMs = mutableMapOf<String, Long>()

    /** A send to [userIdHex] found no route. */
    @Synchronized
    fun noteUnrouted(userIdHex: String, nowMs: Long) {
        if (waitingSinceMs.size >= MAX_TRACKED) prune(nowMs)
        if (waitingSinceMs.size >= MAX_TRACKED) return
        waitingSinceMs[userIdHex] = nowMs
    }

    /**
     * A link to [userIdHex] is up. Digest sync carries the backlog from here,
     * so this contact no longer needs slot priority to be reached.
     */
    @Synchronized
    fun noteRouted(userIdHex: String) {
        waitingSinceMs.remove(userIdHex)
    }

    @Synchronized
    fun isWaiting(userIdHex: String, nowMs: Long): Boolean {
        val since = waitingSinceMs[userIdHex] ?: return false
        if (nowMs - since > ttlMs) {
            waitingSinceMs.remove(userIdHex)
            return false
        }
        return true
    }

    @Synchronized
    fun clear() = waitingSinceMs.clear()

    private fun prune(nowMs: Long) {
        waitingSinceMs.entries.removeAll { nowMs - it.value > ttlMs }
    }

    companion object {
        const val DEFAULT_TTL_MS = 15 * 60_000L
        private const val MAX_TRACKED = 512

        /** Process-wide instance the router and the BLE central share. */
        val shared = PendingPeerMail()
    }
}
