package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.coreLanOwnDeviceSearchSince

/**
 * A stable identity for this person's device roster: its device ids, hex
 * encoded and sorted.
 *
 * Compared, never parsed. [OwnDeviceSearchWindow] only needs to know whether
 * the roster in front of it is the one it saw last — and a *removal* is the
 * change it most needs to notice, which no count of missing siblings can show
 * it, because a removal makes that count smaller.
 */
internal fun ownRosterFingerprint(deviceIds: List<ByteArray>): String = deviceIds
    .map { id -> id.joinToString("") { byte -> "%02x".format(byte) } }
    .sorted()
    .joinToString(",")

/**
 * The bounded window during which this phone sweeps the local subnet looking
 * for one of this person's own devices (`specs/multi-device-v1.md` §10 step 5).
 *
 * A device of this person's own shares their user id, so it has no contact row
 * and can never appear in the LAN transport's contact-side sweep motive however
 * long it waits. Without a motive of its own, mDNS is the only channel between
 * two phones of one person — and one stale mDNS record is the whole of the
 * field failure this exists to prevent.
 *
 * It is a *window* rather than the obvious "a sibling is missing" test because
 * that test never stops being true: a second phone that is switched off, or
 * left at home, is missing forever, so the sweep gate would stand open forever
 * and the planner would hand out a `/24` sweep every five minutes on every
 * Wi-Fi, on battery, for as long as the app runs. (It is not even satisfiable
 * for a person with three devices: the transport keeps at most one own-device
 * link at a time.) The contact-side motive decays for exactly the same reason;
 * this is the same discipline applied to the new one.
 *
 * Core owns the rule and the window ([coreLanOwnDeviceSearchSince]); this holds
 * the two facts it compares against — the roster last observed and the shortfall
 * last observed — and answers the transport's gate.
 *
 * Methods are @Synchronized leaf-monitor style: [observe] runs on the store
 * executor, [isLive] on the LAN transport's main handler.
 */
internal class OwnDeviceSearchWindow {

    private var searchSinceMs: Long? = null
    private var lastRosterFingerprint: String? = null
    private var lastUnlinkedOwnDevices: Int = 0

    /**
     * Fold in what this phone can see right now: [rosterFingerprint] identifies
     * this person's device roster, [unlinkedOwnDevices] is how many of its
     * devices have no own-device link.
     */
    @Synchronized
    fun observe(rosterFingerprint: String, unlinkedOwnDevices: Int, nowMs: Long) {
        val shortfall = unlinkedOwnDevices.coerceAtLeast(0)
        val rosterChanged = lastRosterFingerprint != rosterFingerprint
        lastRosterFingerprint = rosterFingerprint
        searchSinceMs = coreLanOwnDeviceSearchSince(
            previousSinceMs = searchSinceMs,
            rosterChanged = rosterChanged,
            unlinkedOwnDevices = shortfall.toUInt(),
            previousUnlinkedOwnDevices = lastUnlinkedOwnDevices.toUInt(),
            nowMs = nowMs,
        )
        lastUnlinkedOwnDevices = shortfall
    }

    /**
     * A fresh reason to search that [observe] cannot see for itself: joining a
     * Wi-Fi network, where every peer has to be found again from nothing.
     */
    @Synchronized
    fun rearm(nowMs: Long) {
        searchSinceMs = nowMs
    }

    /** Whether the sweep gate may treat "a device of ours is missing" as a motive. */
    @Synchronized
    fun isLive(): Boolean = searchSinceMs != null

    @Synchronized
    fun clear() {
        searchSinceMs = null
        lastRosterFingerprint = null
        lastUnlinkedOwnDevices = 0
    }
}
