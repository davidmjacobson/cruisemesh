package com.cruisemesh.app.mesh

/**
 * What [BlePeripheral] should do with a central that just subscribed to our
 * GATT server's outbound characteristic.
 *
 * [activeCount] is the number of inbound links held *after* the decision, so a
 * log line can report the cap state without a second query racing it.
 */
sealed interface PeripheralAdmissionDecision {
    val activeCount: Int

    /** A free slot was taken for this address; track the link and serve it. */
    data class Admitted(override val activeCount: Int) : PeripheralAdmissionDecision

    /**
     * This address already holds a slot. A repeat CCCD-enable write for a link
     * we are already serving must not consume a second slot, and must
     * certainly not be rejected -- an established link is never severed by the
     * cap (see [PeripheralLinkAdmission]).
     */
    data class AlreadyHeld(override val activeCount: Int) : PeripheralAdmissionDecision

    /** Every slot is held by an older link; this newest one is turned away. */
    data class Rejected(override val activeCount: Int) : PeripheralAdmissionDecision
}

/**
 * The inbound (peripheral-role) half of the ACL-slot budget that
 * `MAX_CENTRAL_LINKS` rations on the outbound side, extracted as a plain
 * class with no Android imports so it is unit-testable directly -- the same
 * pattern as [BleAdvertiserStateMachine] / [CentralConnectAdmission].
 *
 * ## Why the peripheral role needs a cap at all
 *
 * [BleCentral] caps the links *we* choose to open (PR #148, after a capture of
 * a house full of family phones produced ~1 connect/s and 488 status=133
 * failures in seven minutes). Nothing capped the other direction: every
 * central that found our advertisement got a link, and those links come out of
 * the same ~7-8 concurrent ACL connections the controller offers both roles.
 * So in a dense fleet the inbound side could quietly consume the very
 * headroom the central side was carefully leaving free, and the links this
 * phone deliberately chose would starve for slots it never agreed to give
 * away.
 *
 * ## Why admissions, and only admissions
 *
 * This caps *admissions*: at the margin the newest arrival is turned away and
 * every already-established link is left alone. Evicting instead -- dropping
 * the oldest, or the weakest -- would make a dense fleet churn forever, since
 * every phone would keep displacing every other phone's links and no link set
 * would ever settle. Turning the newest away converges instead: a rejected
 * central sees an ordinary disconnect, [ReconnectBackoffTracker] backs its
 * retries off, and it reaches this phone over the *outbound* half of the
 * dual-role link (or via a different mule) rather than fighting for an inbound
 * slot. Only a link that genuinely goes away frees a slot ([release]).
 *
 * ## What a rejection deliberately does not do
 *
 * A phone at cap keeps advertising. It is still a useful DTN carrier for
 * everyone it *is* linked to, other peers can still reach it by carrying via
 * someone else, and going dark would additionally take the phone out of the
 * fleet for the peers whose links it already holds -- see
 * [BleAdvertiserStateMachine] for how expensive an unadvertised phone is.
 *
 * ## When the decision is made
 *
 * Not at `STATE_CONNECTED`, which is the obvious place and the wrong one. That
 * callback fires for *every* central that opens a GATT connection to this
 * phone -- a paired watch, LE earbuds, a car head unit, anything -- not only
 * for ones that touch our service. Deciding there would spend a mesh slot on a
 * device that will never write our CCCD (a watch holds one all day), and at the
 * cap it would aim `cancelConnection` at a link this app does not own. The same
 * shape catches mesh peers whose connect stalls before they subscribe (the
 * status=133 doomed connects [BleCentral] describes): a slot held until the
 * supervision timeout by something that never becomes a route.
 *
 * The decision is made at the CCCD-enable write instead -- the first moment a
 * connection is known to be a mesh client at all, and the moment before which
 * no inbound slot is worth spending. The refusal also has a natural answer
 * there: the subscribe request itself is failed, which is exactly what the far
 * side's central role needs to hear.
 *
 * ## Known limitation: the decision is never revisited
 *
 * The cap cannot flex for "this link is the peer's only route", in two stages.
 *
 * At admission it *cannot* know: the CCCD write still precedes the HELLO
 * exchange in both directions, so nothing is yet known about who the peer is --
 * the identity arrives later in the HELLO, and the BLE address in hand is a
 * rotating RPA -- and by the time `MeshRouter` could say whether a sibling
 * route exists, the slot has already been granted or refused.
 *
 * The sharper half is that a second later it *could* know and still does not.
 * Slots are keyed by BLE address and released only by a real teardown, so an
 * inbound link that #266's collapse-by-authenticated-peer has since superseded
 * -- the peer is also on LAN, or its own outbound BLE half won election --
 * keeps its slot for the life of the link even though `MeshRouter` never
 * selects it and nothing bulk ever flows over it. Such a link accumulates no
 * notify failures (it is never notified) and there is no peripheral-side
 * watchdog, so nothing retires it. Three of those can hold the whole inbound
 * budget against the one peer a spare inbound slot exists to serve: a BLE-only
 * peer with no other way in.
 *
 * Closing that means either evicting superseded links or releasing their slots
 * while the ACL stays up. The first re-opens the churn this class is built to
 * avoid, since route election flaps and each flap would drop a link; the second
 * over-subscribes the very ACL pool the cap protects. Both want a peripheral
 * link-health watchdog that does not exist yet, so this ships with the cap
 * simple and the limitation stated. Its practical weight is small: it needs a
 * phone whose inbound links are *all* superseded at once, and the excluded peer
 * still reaches this phone over its own inbound half or by carrying via
 * another mule.
 *
 * All methods are `@Synchronized`: GATT server callbacks arrive on arbitrary
 * binder threads. This is a leaf monitor -- it never calls out -- so it cannot
 * deadlock with [BlePeripheral]'s own locks.
 */
class PeripheralLinkAdmission(private val maxLinks: Int) {
    init {
        require(maxLinks > 0) { "maxLinks must be positive" }
    }

    private val held = mutableSetOf<String>()

    /**
     * Decide whether [address] may hold an inbound link. Idempotent for an
     * address that already holds one.
     */
    @Synchronized
    fun admit(address: String): PeripheralAdmissionDecision = when {
        address in held -> PeripheralAdmissionDecision.AlreadyHeld(held.size)
        held.size >= maxLinks -> PeripheralAdmissionDecision.Rejected(held.size)
        else -> {
            held += address
            PeripheralAdmissionDecision.Admitted(held.size)
        }
    }

    /**
     * Record a slot for [address] even if that puts the count over the cap, and
     * return the resulting count.
     *
     * Not an admission decision and never called on the admission path: this is
     * reconciliation for the one case where the radio and the ledger disagree --
     * a central this class turned away that could not actually be disconnected
     * (see `BlePeripheral.adoptUndroppableCentral`). The controller is holding
     * that ACL slot whatever this class thinks, and a ledger that calls it free
     * would over-subscribe the pool the cap exists to protect, so the honest
     * count is the one that includes it. [release] frees it like any other.
     */
    @Synchronized
    fun forceHold(address: String): Int {
        held += address
        return held.size
    }

    /**
     * Free [address]'s slot. Returns whether it actually held one, so a caller
     * can tell a real teardown from a repeat of one (the GATT stack delivers
     * duplicate disconnect callbacks; see [BlePeripheral.tearDownLink]).
     */
    @Synchronized
    fun release(address: String): Boolean = held.remove(address)

    @Synchronized
    fun holds(address: String): Boolean = address in held

    @Synchronized
    fun activeCount(): Int = held.size

    @Synchronized
    fun clearAll() = held.clear()
}

/**
 * Which centrals [PeripheralLinkAdmission] turned away and are still being
 * dropped, and -- the part that needs a class rather than a set -- *which
 * rejection* each pending drop attempt belongs to.
 *
 * A turned-away central is disconnected by a short ladder of posted
 * `cancelConnection` attempts, because one call does not reliably drop an ACL
 * the far side opened. Those posts outlive the connection they were issued for.
 * BLE resolvable private addresses only rotate every ~15 minutes, so the same
 * central reconnecting seconds later usually reuses the same address, and a
 * plain "is this address rejected" check cannot tell the two apart: an older
 * ladder would keep acting on the newer connection, and would reach its
 * end-of-ladder adoption after only some of its own attempts had run against
 * that ACL -- force-holding a slot over the cap while the drop path had not
 * actually been exhausted.
 *
 * Every rejection therefore gets a process-unique generation. A ladder carries
 * the generation it started with and retires the moment that generation stops
 * being the address's current one, so a stale ladder can neither drop a link it
 * does not own nor adopt one prematurely. Generations come from one
 * monotonically increasing counter rather than a per-address one, so a
 * disconnect-and-reconnect cycle can never hand a stale ladder a number that
 * matches again.
 *
 * All methods are `@Synchronized`: GATT server callbacks and the ladder's own
 * posted runnables run on different threads. This is a leaf monitor -- it never
 * calls out -- so it cannot deadlock with [BlePeripheral]'s own locks.
 */
class PeripheralRejectionLedger {
    private var sequence = 0L
    private val rejectedAt = mutableMapOf<String, Long>()

    /**
     * Record [address] as turned away and return the generation the drop ladder
     * for this rejection must carry. Replaces any earlier rejection of the same
     * address, which is what retires that one's ladder.
     */
    @Synchronized
    fun reject(address: String): Long {
        sequence += 1
        rejectedAt[address] = sequence
        return sequence
    }

    /** Whether [address] is currently being turned away, under any generation. */
    @Synchronized
    fun isRejected(address: String): Boolean = address in rejectedAt

    /**
     * Whether [address] is still rejected under exactly [generation] -- the
     * guard every posted drop attempt runs before touching the radio.
     */
    @Synchronized
    fun ownsRejection(address: String, generation: Long): Boolean = rejectedAt[address] == generation

    /**
     * End [address]'s rejection only if [generation] is the one that owns it,
     * returning whether it did. Used by the end of the ladder, where acting on
     * someone else's rejection would adopt a link over the cap.
     */
    @Synchronized
    fun clearIfOwned(address: String, generation: Long): Boolean =
        if (rejectedAt[address] == generation) {
            rejectedAt.remove(address)
            true
        } else {
            false
        }

    /**
     * End [address]'s rejection whatever generation owns it -- a disconnect or a
     * fresh connection, both of which make every pending ladder stale.
     */
    @Synchronized
    fun clear(address: String): Boolean = rejectedAt.remove(address) != null

    @Synchronized
    fun clearAll() = rejectedAt.clear()

    /** Visible for tests: how many addresses are currently being turned away. */
    @Synchronized
    fun rejectedCount(): Int = rejectedAt.size
}

/**
 * Per-address brake on the HELLO-triggered sync burst after this phone tore an
 * inbound link down because its own notifications were being rejected.
 *
 * ## The loop this exists to break
 *
 * When a central subscribes, the peripheral answers the peer's HELLO with the
 * carried-envelope drain plus the §7.3 digest -- a burst that the 2026-07-17
 * capture showed queuing 19+ frames, several KB, for one address at once. If
 * the controller rejects enough of those notifications in a row,
 * [NotifyFailureTracker] concludes the link is dead and [BlePeripheral] tears
 * it down. Nothing then stood between that teardown and the same central
 * reconnecting on its very next scan hit and triggering the identical burst:
 * the reconnect is free (we re-advertise immediately after every teardown, as
 * we must), and the burst is what broke the link in the first place. No such
 * thrash appeared in the capture -- the peers walked out of range first -- but
 * nothing bounded it either.
 *
 * ## Suppress the spray, not the connection
 *
 * A reconnect is *allowed* during the window: refusing the connection would
 * cost this phone a DTN carrier and make the peer's own reconnect backoff
 * escalate against a peer that is perfectly reachable. What the window
 * suppresses is the multi-KB half of the exchange. The two HELLO frames still
 * go out -- tens of bytes, and they are what keeps `MeshRouter`'s
 * address-to-identity mapping honest, without which the link is useless for
 * anything at all.
 *
 * Both outbound halves have to be gated for that to mean anything, and this is
 * the part that is easy to get wrong. Holding back only what the peer's HELLO
 * triggers brakes nothing: our own HELLO still goes out (it must), the peer
 * answers a HELLO with its DIGEST, and our response to *that* is the bigger
 * burst of the two -- receipts, every 1:1 message its watermark says it is
 * missing, every group envelope we authored, and the carry-queue spray. So
 * `MeshService` consults this window in both `handleHello` and `handleDigest`.
 *
 * ## Nothing is dropped, only delayed
 *
 * A suppressed burst is re-armed for when the window lapses, and it re-enters
 * through the same coalescing resume the failover debounce uses, so a peer
 * whose link genuinely settled gets its carry drain and digest one window
 * late rather than not at all -- one re-arm per peer, not one per gated frame.
 * That resume is also where the window is read, so *every* way into it is
 * braked (a failover from a dying sibling link, a route promotion, the
 * deferral's own re-entry), not just the two frame handlers that arm it.
 *
 * A peer *digest* gated by the window is held too, not thrown away. Dropping
 * it looks safe -- nothing a digest says is written to our own store, and the
 * peer re-sends one on its own maintenance tick -- but the peer's digest is the
 * only thing that triggers the receipts we owe it, the messages its watermark
 * says it is missing, and the confirmation that lets us retire carried copies
 * it already holds. Dropping it would push all of that out to that maintenance
 * tick, which is minutes, on exactly the link that just recovered. So the
 * gated digest's contents are stashed per peer and replayed when the deferral
 * fires, against whatever route is elected by then.
 *
 * ## Sizing
 *
 * [DEFAULT_WINDOW_MS] matches [ReconnectBackoffTracker.INITIAL_BACKOFF_MS]:
 * that is what the far side's central role already waits before its first
 * retry to a known-failed address, so the two brakes describe the same "let
 * this settle" interval instead of fighting each other. Nothing waits on the
 * digest maintenance interval, so the window is the whole delay a recovered
 * peer sees.
 *
 * `nowMs` must come from a monotonic clock (`SystemClock.elapsedRealtime()`),
 * the same one the deferral's timer counts down on -- measuring the window on
 * the wall clock lets a clock correction expire it early or hold it open
 * indefinitely, exactly as [FailoverResumeDebounce] documents.
 *
 * All methods are `@Synchronized` (binder threads) and this is a leaf monitor.
 */
class PeripheralSprayCooldown(private val windowMs: Long = DEFAULT_WINDOW_MS) {

    companion object {
        /** See the sizing note in this class's doc. */
        const val DEFAULT_WINDOW_MS = 5_000L
    }

    init {
        require(windowMs > 0) { "windowMs must be positive" }
    }

    /** Address -> monotonic ms at which its suppression window lapses. */
    private val lapsesAtMs = mutableMapOf<String, Long>()

    /**
     * A link to [address] was just torn down because our notifications to it
     * were failing. Starts (or restarts) that address's window.
     */
    @Synchronized
    fun armAfterRejectTeardown(address: String, nowMs: Long) {
        pruneExpired(nowMs)
        lapsesAtMs[address] = nowMs + windowMs
    }

    /**
     * How much longer the sync burst to [address] must be held back, or 0 when
     * it may go out now. An expired entry is dropped as it is read, which is
     * also what bounds this map: an address is remembered for one window, not
     * for the life of the process.
     */
    @Synchronized
    fun deferralMs(address: String, nowMs: Long): Long {
        val lapsesAt = lapsesAtMs[address] ?: return 0L
        val remaining = lapsesAt - nowMs
        if (remaining <= 0L) {
            lapsesAtMs.remove(address)
            return 0L
        }
        return remaining
    }

    /** Visible for tests: how many addresses are currently remembered. */
    @Synchronized
    fun trackedAddressCount(): Int = lapsesAtMs.size

    @Synchronized
    fun clearAll() = lapsesAtMs.clear()

    private fun pruneExpired(nowMs: Long) {
        lapsesAtMs.entries.removeAll { it.value - nowMs <= 0L }
    }
}
