package com.cruisemesh.app.mesh

/**
 * How much a peer is worth one of the central role's scarce link slots.
 *
 * Resolved by the caller from a user id, so this class stays free of any
 * store or Android dependency (see [CentralConnectAdmission.standingOf]).
 */
enum class BlePeerStanding {
    /** Never seen HELLO from this identity, or seen and not in our contacts. */
    UNKNOWN,

    /** An accepted contact with nothing queued for them right now. */
    CONTACT,

    /** An accepted contact we currently hold undeliverable outbound mail for. */
    CONTACT_WITH_MAIL,
}

/**
 * Slot policy for [BleCentral]'s outbound links: who gets one of the few
 * central connections, who is our own advertisement, who is a duplicate of
 * somebody already holding a slot, and who may be pushed out to make room.
 * Plain class, no Android imports, unit-tested directly (the CLAUDE.md "pure
 * policy logic" shape, same as [RadioPowerPolicy] / [GattWriteQueue] /
 * [PeripheralLinkAdmission]).
 *
 * ## Slots are reserved before the framework call
 *
 * A reserved address consumes one of [maxActive] slots immediately, before
 * `connectGatt` is even posted. That keeps a burst of scan callbacks from all
 * observing the same stale `connections.size`, and the session carried by a
 * [Reservation] lets `stop()` invalidate work that is already queued or
 * inside `connectGatt` without killing the reusable worker thread.
 *
 * ## Identity, not address
 *
 * The budget used to be counted by BLE address alone, which is not a peer:
 * addresses are resolvable private addresses that rotate every ~15 minutes,
 * so one phone can hold several slots at once under several addresses while a
 * contact with mail waiting never gets one. Every candidate is therefore
 * resolved to an *identity key* first ([identityKeyFor]) and at most one slot
 * is ever held per key:
 *
 *  - `user:<hex>` once HELLO has named the peer ([onIdentified]). Strongest;
 *    survives address rotation and collapses the two keys below onto itself.
 *  - `adv:<hex>` from the scan-response service data every Android build puts
 *    in its advertisement (`MeshConstants.LOCAL_INSTANCE_ID`). A random
 *    per-process token, so it is not an identity anyone can act on, but it is
 *    stable across that peer's address rotation for the life of its process,
 *    which is exactly what dedupe needs. Once a token has been seen HELLO in
 *    as a user id that mapping is remembered ([userIdByToken]), so the next
 *    advertisement from the same token resolves straight to `user:` and
 *    priority applies from the *first* sight of a rotated address.
 *  - `addr:<address>` when the advertisement carries no service data at all.
 *    This is the honest fallback, not a fudge: iOS cannot advertise service
 *    data (CoreBluetooth has no API for it), so an iPhone peer is genuinely
 *    unidentifiable until it has connected and sent HELLO. Those candidates
 *    are deduped *after* identification instead -- [onIdentified] returns the
 *    redundant link for the caller to drop, keeping the invariant ("one slot
 *    per identity") even though it could not be enforced up front.
 *
 * Everything remembered here stays here. A token, a user id, and the mapping
 * between them are used only to decide who this phone dials next; nothing
 * learned from one peer is ever handed to another (CLAUDE.md's endpoint
 * privacy invariant).
 *
 * ## Never our own listener
 *
 * A peer whose advertisement carries [selfInstanceToken] is this process's
 * own peripheral role and is refused. The old guard did only that, reading
 * the service data off the scan result in hand -- and a legacy advertisement
 * is two PDUs, so the same self-advertisement also arrives as a bare ADV_IND
 * with no scan-response payload merged in, where the guard saw no token and
 * dialled. That is the every-few-seconds self-dial seen in the field. The
 * classification is therefore *sticky per address* ([selfAddresses]): one
 * sighting carrying our token condemns that address for as long as it is
 * remembered, whether or not the next scan result repeats the payload. Our
 * own address rotates like anyone's, so the worst case becomes one wasted
 * dial per rotation instead of one every scan window.
 *
 * The token stays per *process* on purpose. It is not a device or build
 * constant: a constant shared by two packages of this app would also be
 * shared by every phone running it, and this guard would then reject the
 * entire mesh. The corollary is deliberate -- a debug build and a release
 * build installed side by side hold two distinct tokens and treat each other
 * as ordinary peers, which is what makes that pair usable as a test rig.
 *
 * ## Priority, with a floor under strangers
 *
 * With a free slot, anybody who is not self and not a duplicate is admitted:
 * ranking only decides *contested* slots. At capacity a candidate may take a
 * slot from a strictly lower-ranked holder ([rank]: mail > contact >
 * unknown), which is what lets a contact with queued mail displace a stranger
 * on a crowded deck.
 *
 * Two rules keep that from starving new friends, who by construction always
 * arrive as strangers:
 *
 *  1. Unknowns keep at least [unknownReserve] slots. A contact -- with mail
 *     or not -- may not evict an unknown holder if doing so would drop the
 *     unknown-held count below that floor.
 *  2. While unknowns hold fewer than their reserve, an unknown candidate may
 *     itself take a slot from an idle contact (never from a contact with
 *     mail). So a phone that has filled its budget with quiet contacts still
 *     lets the person standing next to you connect for the first time.
 *
 * Nothing is evicted before it has held its slot for [minHoldMs]; without
 * that, two peers of adjacent rank would trade the same slot on every scan
 * callback, which is the churn this whole class exists to bound.
 */
internal class CentralConnectAdmission(
    private val maxActive: Int,
    /**
     * Hex of this process's own advertised service data; see "Never our own
     * listener" above for why it is per-process and never a build constant.
     */
    private val selfInstanceToken: String? = null,
    private val unknownReserve: Int = DEFAULT_UNKNOWN_RESERVE,
    private val minHoldMs: Long = DEFAULT_MIN_HOLD_MS,
    private val standingOf: (userIdHex: String) -> BlePeerStanding = { BlePeerStanding.UNKNOWN },
) {
    init {
        require(maxActive > 0)
        require(unknownReserve >= 0)
    }

    class Reservation internal constructor(
        val address: String,
        internal val session: Long,
    )

    /** Why [tryReserve] did or did not hand out a slot. */
    enum class Outcome {
        /** A free slot was available. */
        ADMITTED,

        /** At capacity; [Attempt.preemptedAddress] was evicted to make room. */
        PREEMPTED,

        /** This advertisement is our own peripheral's. */
        SELF,

        /** Another live slot already holds this peer's identity. */
        DUPLICATE_IDENTITY,

        /** This exact address already holds a slot. */
        ALREADY_TRACKED,

        /** Full, and nothing here ranks low enough to give way. */
        AT_CAPACITY,

        /** The central role is not running. */
        STOPPED,
    }

    data class Attempt(
        val reservation: Reservation?,
        val outcome: Outcome,
        val activeCount: Int,
        /** The link the caller must tear down before using [reservation]. */
        val preemptedAddress: String? = null,
    ) {
        val atCapacity: Boolean get() = outcome == Outcome.AT_CAPACITY
    }

    private enum class Phase { PENDING, CONNECTING, CONNECTED }

    private class Entry(
        val session: Long,
        val instanceToken: String?,
        var identityKey: String,
        var phase: Phase,
        val heldSinceMs: Long,
    )

    private var running = false
    private var session = 0L
    private val entries = mutableMapOf<String, Entry>()

    /**
     * Advertised token -> the user id it turned out to belong to, remembered
     * across address rotations and across a stop/start of the role (a peer's
     * token only changes when its process does). Bounded so a very dense
     * fleet cannot grow it without limit.
     */
    private val userIdByToken = lruKeyedMap<String>(MAX_REMEMBERED_TOKENS)

    /**
     * Addresses seen advertising [selfInstanceToken]. See "Never our own
     * listener": this is what keeps the guard working on the half of our own
     * advertisement that arrives with no service data attached. Bounded --
     * only our current and recently rotated addresses matter, and an address
     * forgotten from here costs at worst one more dial.
     */
    private val selfAddresses = lruKeyedMap<Unit>(MAX_REMEMBERED_SELF_ADDRESSES)

    @Synchronized
    fun startSession() {
        if (running) return
        session = session.nextGeneration()
        running = true
        entries.clear()
    }

    @Synchronized
    fun stopSession() {
        running = false
        session = session.nextGeneration()
        entries.clear()
    }

    /**
     * The stable key this advertisement belongs to. Callers key their
     * per-peer bookkeeping (notably the reconnect backoff) on this rather
     * than on the address, so a peer that rotates through a dozen addresses
     * accumulates one failure history instead of a dozen fresh ones -- the
     * reason the old per-address backoff never engaged against a churning
     * fleet.
     */
    @Synchronized
    fun identityKeyFor(address: String, instanceToken: String?): String = when {
        instanceToken == null -> ADDRESS_PREFIX + address
        else -> userIdByToken[instanceToken]?.let { USER_PREFIX + it } ?: (ADVERT_PREFIX + instanceToken)
    }

    /**
     * The key a live link's history is filed under, for the failure and
     * success paths that hold only a `BluetoothGatt` (that is, only an
     * address). Follows the link's own upgrade to `user:` in [onIdentified],
     * so a success recorded after HELLO clears the very counter the next scan
     * sighting of that peer will consult.
     */
    @Synchronized
    fun identityKeyOf(address: String): String =
        entries[address]?.identityKey ?: (ADDRESS_PREFIX + address)

    /**
     * Reserve capacity for [address], including while its worker call waits.
     * [instanceToken] is the hex of the advertisement's service data, or null
     * when it carried none.
     */
    @Synchronized
    fun tryReserve(address: String, instanceToken: String?, nowMs: Long): Attempt {
        if (!running) return Attempt(null, Outcome.STOPPED, entries.size)
        if (instanceToken != null && instanceToken == selfInstanceToken) {
            selfAddresses[address] = Unit
            return Attempt(null, Outcome.SELF, entries.size)
        }
        if (address in selfAddresses) return Attempt(null, Outcome.SELF, entries.size)
        if (address in entries) return Attempt(null, Outcome.ALREADY_TRACKED, entries.size)
        val key = identityKeyFor(address, instanceToken)
        if (entries.values.any { it.identityKey == key }) {
            return Attempt(null, Outcome.DUPLICATE_IDENTITY, entries.size)
        }
        if (entries.size < maxActive) {
            entries[address] = Entry(session, instanceToken, key, Phase.PENDING, nowMs)
            return Attempt(Reservation(address, session), Outcome.ADMITTED, entries.size)
        }
        val victim = chooseVictim(standingFor(key), nowMs)
            ?: return Attempt(null, Outcome.AT_CAPACITY, entries.size)
        entries.remove(victim)
        entries[address] = Entry(session, instanceToken, key, Phase.PENDING, nowMs)
        return Attempt(Reservation(address, session), Outcome.PREEMPTED, entries.size, victim)
    }

    /** Claim queued work immediately before making the framework call. */
    @Synchronized
    fun beginConnect(reservation: Reservation): Boolean {
        val entry = entryFor(reservation) ?: return false
        if (entry.phase != Phase.PENDING) return false
        entry.phase = Phase.CONNECTING
        return true
    }

    /** Publish a returned GATT only if this reservation's session is live. */
    @Synchronized
    fun completeConnect(reservation: Reservation): Boolean {
        val entry = entryFor(reservation) ?: return false
        if (entry.phase != Phase.CONNECTING) return false
        entry.phase = Phase.CONNECTED
        return true
    }

    /** Release work that was rejected by the handler or failed before publish. */
    @Synchronized
    fun cancel(reservation: Reservation) {
        val entry = entryFor(reservation) ?: return
        if (entry.phase != Phase.CONNECTED) {
            entries.remove(reservation.address)
        }
    }

    @Synchronized
    fun disconnect(address: String) {
        entries.remove(address)
    }

    /**
     * HELLO named the peer on [address]. Binds that link (and, for later
     * sightings, its advertised token) to [userIdHex] and returns the address
     * of a link that must now be torn down because it is a second slot for
     * the same identity -- or null when this link is the only one.
     *
     * This is the post-connect half of the dedupe: a peer whose advertisement
     * carries no service data cannot be recognised before connecting, so two
     * of its rotated addresses can each win a slot. Whichever of the pair has
     * held its slot for less time is the one dropped; the older link is the
     * one the router has already been using.
     */
    @Synchronized
    fun onIdentified(address: String, userIdHex: String): String? {
        val entry = entries[address] ?: return null
        entry.instanceToken?.let { userIdByToken[it] = userIdHex }
        val key = USER_PREFIX + userIdHex
        val duplicate = entries.entries.firstOrNull { it.key != address && it.value.identityKey == key }
        entry.identityKey = key
        if (duplicate == null) return null
        val drop = if (duplicate.value.heldSinceMs <= entry.heldSinceMs) address else duplicate.key
        entries.remove(drop)
        return drop
    }

    /** Live slot count, for logging. */
    @Synchronized
    fun activeCount(): Int = entries.size

    private fun chooseVictim(candidate: BlePeerStanding, nowMs: Long): String? {
        val unknownHeld = entries.count { standingFor(it.value.identityKey) == BlePeerStanding.UNKNOWN }
        val candidateRank = rank(candidate)
        return entries.entries
            .asSequence()
            .filter { nowMs - it.value.heldSinceMs >= minHoldMs }
            .filter { (_, entry) ->
                val holder = standingFor(entry.identityKey)
                when {
                    // Rule 1: unknowns keep their floor.
                    holder == BlePeerStanding.UNKNOWN && unknownHeld <= unknownReserve -> false
                    candidateRank > rank(holder) -> true
                    // Rule 2: a stranger's first connection, taken from an
                    // idle contact rather than never happening at all.
                    candidate == BlePeerStanding.UNKNOWN &&
                        unknownHeld < unknownReserve &&
                        holder == BlePeerStanding.CONTACT -> true
                    else -> false
                }
            }
            .minWithOrNull(
                compareBy(
                    { rank(standingFor(it.value.identityKey)) },
                    // Among equals, the slot held longest has had its turn;
                    // taking the freshest instead would undo the connect that
                    // has only just succeeded.
                    { it.value.heldSinceMs },
                ),
            )
            ?.key
    }

    private fun standingFor(identityKey: String): BlePeerStanding =
        if (identityKey.startsWith(USER_PREFIX)) {
            standingOf(identityKey.removePrefix(USER_PREFIX))
        } else {
            BlePeerStanding.UNKNOWN
        }

    private fun rank(standing: BlePeerStanding): Int = when (standing) {
        BlePeerStanding.UNKNOWN -> 1
        BlePeerStanding.CONTACT -> 2
        BlePeerStanding.CONTACT_WITH_MAIL -> 3
    }

    private fun entryFor(reservation: Reservation): Entry? {
        if (!running || reservation.session != session) return null
        return entries[reservation.address]?.takeIf { it.session == reservation.session }
    }

    companion object {
        const val DEFAULT_UNKNOWN_RESERVE = 1
        const val DEFAULT_MIN_HOLD_MS = 20_000L
        private const val MAX_REMEMBERED_TOKENS = 256
        private const val MAX_REMEMBERED_SELF_ADDRESSES = 32
        private const val USER_PREFIX = "user:"
        private const val ADVERT_PREFIX = "adv:"
        private const val ADDRESS_PREFIX = "addr:"

        /** Access-ordered map that forgets its least recently used key. */
        private fun <V> lruKeyedMap(capacity: Int): MutableMap<String, V> =
            object : LinkedHashMap<String, V>(16, 0.75f, true) {
                override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, V>): Boolean =
                    size > capacity
            }
    }
}

private fun Long.nextGeneration(): Long = if (this == Long.MAX_VALUE) 0L else this + 1L
