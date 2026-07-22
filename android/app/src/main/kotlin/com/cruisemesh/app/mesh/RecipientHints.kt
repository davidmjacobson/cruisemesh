package com.cruisemesh.app.mesh

import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.computeRecipientHint

/**
 * How many recent day-numbers to hash a peer's UserID against when matching
 * carried envelopes (DESIGN.md §5.3 carry queue, §6.4 `recipient_hint`). A
 * `recipient_hint` is `BLAKE2b-8(UserID || day-number)` where the day-number
 * is the envelope's *creation* day; since envelopes live `DEFAULT_EXPIRY_MS`
 * (7 days) and an unexpired one was created at most 7 days ago, hashing a
 * peer's UserID against today back through 7 days ago covers every day-salt a
 * still-carried envelope for them could have used. Mirrors core's
 * `DEFAULT_EXPIRY_MS` / `MS_PER_DAY`.
 */
private const val CARRY_HINT_DAY_WINDOW: Long = 7
private const val PRESENCE_HINT_DAY_WINDOW: Long = 3

/**
 * FA15: the shared `recipient_hint` directory. Every "which hints could this
 * userId/group match" computation MeshService, [RelaySyncEngine], and
 * [InboundEnvelopeProcessor] make lives here, so the relay-sync and
 * envelope-processing pipelines agree on one source of truth for hint
 * derivation. Pure store-backed computation — no Android imports, safe to
 * call from any thread the store itself tolerates.
 */
internal class RecipientHints(private val store: MessageStore) {

    /**
     * The `recipient_hint`s [userId] could match for a still-carriable
     * envelope: their UserID hashed against each day-number from today back
     * through [CARRY_HINT_DAY_WINDOW] days (DESIGN.md §6.4's daily-rotating
     * hint over the §5.3 expiry window).
     */
    fun recentHintsFor(userId: ByteArray, now: Long): List<ByteArray> =
        (0..CARRY_HINT_DAY_WINDOW).map { daysAgo ->
            computeRecipientHint(userId, now - daysAgo * MS_PER_DAY)
        }

    fun recentPresenceHintsFor(userId: ByteArray, now: Long): List<ByteArray> =
        (0..PRESENCE_HINT_DAY_WINDOW).map { daysAgo ->
            computeRecipientHint(userId, now - daysAgo * MS_PER_DAY)
        }

    /** Mail addressed to us: our own hint, plus every group we belong to (DESIGN.md §6.5). */
    fun relayHintsForConfig(
        ownUserId: ByteArray,
        now: Long,
    ): List<ByteArray> {
        val hints = recentHintsFor(ownUserId, now).toMutableList()
        // Pull group-addressed mail for every group we belong to (DESIGN.md §6.5).
        for (group in store.listGroups()) {
            if (group.memberUserIds.any { it.contentEquals(ownUserId) }) {
                hints += recentHintsFor(group.id, now)
            }
        }
        return hints
    }

    /**
     * "Proxy" hints for relay proxy-polling: the recent-day `recipient_hint`s
     * for every contact that isn't us, so an internet-connected phone sitting
     * in a BLE-only contact's cluster can also fetch mail addressed to *them*
     * out of the shared family-token relay partition, then carry it over BLE
     * the rest of the way ([InboundEnvelopeProcessor.carryRelayEnvelope]).
     * Without this, only the contact's own (internet-less) hint would ever be
     * polled, and a 1:1 envelope addressed to them would sit unfetched
     * forever.
     *
     * Bounded by family size: this fetches every contact's hints on every
     * poll pass, so cost grows linearly with the contact list. That's fine
     * for the small family circles this app targets; see the scaling TODO on
     * [RelaySyncEngine.pollRelayMailbox] if that assumption ever changes.
     */
    fun relayProxyHints(
        contacts: List<Contact>,
        ownUserId: ByteArray,
        now: Long,
    ): List<ByteArray> {
        val hints = mutableListOf<ByteArray>()
        for (contact in contacts) {
            if (contact.userId.contentEquals(ownUserId)) continue
            hints += recentHintsFor(contact.userId, now)
        }
        return hints
    }

    /**
     * Combines [selfHints] and [proxyHints] into one fetch list, deduping by
     * content (`ByteArray` has reference equality, so a plain `distinct()`
     * would not catch two equal-content hints -- e.g. a contact hint that
     * happens to coincide with a group hint we already fetch for ourselves).
     */
    fun dedupeHints(selfHints: List<ByteArray>, proxyHints: List<ByteArray>): List<ByteArray> {
        val seen = HashSet<String>(selfHints.size + proxyHints.size)
        val out = ArrayList<ByteArray>(selfHints.size + proxyHints.size)
        for (hint in selfHints + proxyHints) {
            if (seen.add(UserIdHex.encode(hint))) {
                out += hint
            }
        }
        return out
    }

    fun contactMatchingHint(contacts: List<Contact>, hint: ByteArray, now: Long): Contact? {
        contacts.firstOrNull { contact ->
            recentHintsFor(contact.userId, now).any { it.contentEquals(hint) }
        }?.let { return it }
        // Group-addressed carries: upload via any member's relay config.
        for (group in store.listGroups()) {
            if (!recentHintsFor(group.id, now).any { it.contentEquals(hint) }) continue
            for (memberId in group.memberUserIds) {
                val contact = contacts.firstOrNull { it.userId.contentEquals(memberId) }
                if (contact != null) return contact
            }
        }
        return null
    }

    /**
     * `recipient_hint`s the peer can open: their own userId over recent days,
     * plus every imported group they belong to.
     */
    fun deliveryHintsForPeer(peerUserId: ByteArray, now: Long): List<ByteArray> {
        val hints = recentHintsFor(peerUserId, now).toMutableList()
        for (group in store.listGroups()) {
            if (group.memberUserIds.any { it.contentEquals(peerUserId) }) {
                hints += recentHintsFor(group.id, now)
            }
        }
        return hints
    }

    /** True if [hint] matches a known contact or imported group (family traffic). */
    fun hintMatchesKnownTarget(hint: ByteArray, now: Long): Boolean {
        for (contact in store.listContacts()) {
            if (recentHintsFor(contact.userId, now).any { it.contentEquals(hint) }) {
                return true
            }
        }
        return recognizesGroupHint(hint, now)
    }

    private fun recognizesGroupHint(hint: ByteArray, now: Long): Boolean =
        groupMatchingHint(hint, now) != null

    /**
     * The imported group whose recent-day hints include [hint], if any --
     * the group-shaped sibling of [contactMatchingHint], used by the fan-out
     * upload path (specs/group-relay-durability.md §4.2) which needs the
     * group's member list, not just a yes/no.
     */
    fun groupMatchingHint(hint: ByteArray, now: Long): Group? {
        for (group in store.listGroups()) {
            if (recentHintsFor(group.id, now).any { it.contentEquals(hint) }) {
                return group
            }
        }
        return null
    }
}
