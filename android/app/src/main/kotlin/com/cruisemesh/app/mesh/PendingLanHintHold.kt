package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.Frame

/**
 * Pure, unit-testable bounded address->hint hold backing
 * [MeshService.handleLanEndpointHint]'s before-HELLO race (Pixel 10 Pro field
 * log, 2026-07-17): a peer's LAN endpoint hint frame can legitimately arrive
 * on a link before `MeshRouter` has learned that address's userId from its
 * HELLO -- ordinary frame reordering, or (now tolerated rather than
 * mis-diagnosed as a dead link, see [NotifyFailureTracker]) simply a HELLO
 * that hasn't made it through yet. The old code just dropped the hint with
 * no retry, permanently losing the same-Wi-Fi introduction for that
 * connection. This class remembers the most recent hint per address until a
 * HELLO can resolve it (see [MeshService.handleHello], which replays it) or
 * the link disconnects (see [MeshService.onCentralPeerDisconnected] /
 * [MeshService.onPeripheralCentralDisconnected], which clear it) -- bounded
 * to [maxEntries] addresses so a very chatty or malicious link can't grow
 * this without limit. Re-stashing an address (a newer hint arrives before
 * the older one was ever claimed) replaces its entry and counts as the
 * freshest; when over capacity the oldest entry is evicted first.
 *
 * Methods are @Synchronized: [MeshService] gives no single-thread guarantee
 * for frame handling -- hints, HELLOs, and disconnects for different links
 * arrive on different transport/binder threads, and a LinkedHashMap can
 * corrupt structurally under concurrent mutation. Leaf monitor; never calls
 * out.
 */
class PendingLanHintHold(private val maxEntries: Int = MAX_ENTRIES) {
    // LinkedHashMap in insertion order: re-stashing an address removes then
    // re-adds it so it moves to the end (freshest); eviction always takes
    // from the front (oldest).
    private val held = LinkedHashMap<String, Frame.LanEndpoint>()

    /** Stashes [hint] for [address], evicting the oldest held entry if now over capacity. */
    @Synchronized
    fun stash(address: String, hint: Frame.LanEndpoint) {
        held.remove(address)
        held[address] = hint
        while (held.size > maxEntries) {
            val oldest = held.keys.firstOrNull() ?: break
            held.remove(oldest)
        }
    }

    /** Removes and returns the held hint for [address], if any. */
    @Synchronized
    fun take(address: String): Frame.LanEndpoint? = held.remove(address)

    /** [address] disconnected; its held hint (if any) is stale. */
    @Synchronized
    fun clear(address: String) {
        held.remove(address)
    }

    /** Number of addresses currently held, for tests/diagnostics. */
    @Synchronized
    fun size(): Int = held.size

    companion object {
        const val MAX_ENTRIES = 8
    }
}
