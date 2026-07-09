package com.cruisemesh.app.mesh

/**
 * Pure address<->userId mapping backing [MeshRouter] (DESIGN.md §5.2 dual
 * BLE roles, §7.3 interim sync). Kept as a plain class with no Android
 * framework dependencies, following the pattern set by
 * [ReconnectBackoffTracker] and [FrameFraming], so the mapping logic is
 * unit-testable without a device -- see MeshRouterStateTest.
 *
 * A BLE link only becomes "usable" (able to carry a HELLO) at a specific
 * point per role -- see [MeshRouter] -- so [onConnected] and [onDisconnected]
 * are driven by those lifecycle events, and [onHello] is driven by the first
 * frame actually received on that link. Until a HELLO arrives, an address is
 * connected but its userId is unknown (`userIdFor` returns null).
 *
 * Dual-role note: because every phone runs both BLE roles at once, the same
 * physical peer can end up connected under two entries here (one where we
 * dialed them as a central, one where they dialed us as a peripheral). Both
 * entries independently learn the same userId via their own HELLO, and
 * [routeFor] is deliberately indifferent to which one it returns -- either
 * link reaches the same peer, so picking the first match found is correct.
 * When one of the two links drops, [onDisconnected] removes only that
 * entry; the peer stays reachable over the other.
 *
 * Thread-safety: mutators arrive on BLE binder threads while [routeFor] is
 * called from the UI thread's send path, so every method synchronizes on the
 * map. The operations are all tiny (no I/O under the lock).
 */
class MeshRouterState {
    /** Which local BLE role a given remote address is talking to. */
    enum class Transport { CENTRAL, PERIPHERAL }

    private data class Peer(val transport: Transport, var userId: ByteArray?)

    private val peersByAddress = mutableMapOf<String, Peer>()

    /** A link to [address] over [transport] just became usable (able to send/receive frames). */
    fun onConnected(address: String, transport: Transport) {
        synchronized(peersByAddress) {
            peersByAddress[address] = Peer(transport, userId = null)
        }
    }

    /** [address] is no longer connected; forget it so sends never target a dead link. */
    fun onDisconnected(address: String) {
        synchronized(peersByAddress) {
            peersByAddress.remove(address)
        }
    }

    /** Record that the still-connected [address] identified itself as [userId] via a HELLO frame. */
    fun onHello(address: String, userId: ByteArray) {
        synchronized(peersByAddress) {
            peersByAddress[address]?.userId = userId
        }
    }

    /** The userId [address] identified as, if it has sent a HELLO and is still connected. */
    fun userIdFor(address: String): ByteArray? =
        synchronized(peersByAddress) { peersByAddress[address]?.userId }

    /** Which local role [address] is connected under, or null if it isn't currently connected. */
    fun transportFor(address: String): Transport? =
        synchronized(peersByAddress) { peersByAddress[address]?.transport }

    /**
     * A snapshot of every currently connected link as (transport, address)
     * pairs, regardless of whether its userId is known yet. Used by the
     * gossip flood path (DESIGN.md §5.3): a foreign envelope is relayed to
     * every link *except* the one it arrived on, and relaying doesn't need to
     * know who's on the far end -- an unopenable envelope is forwarded blindly
     * and the true recipient (or the next relay) sorts it out. Returns a copy
     * so the caller can iterate without holding the lock.
     */
    fun connectedRoutes(): List<Pair<Transport, String>> =
        synchronized(peersByAddress) {
            peersByAddress.map { (address, peer) -> peer.transport to address }
        }

    /**
     * The transport + address currently usable to reach [userId], or null if
     * no connected link has identified itself as that user yet.
     */
    fun routeFor(userId: ByteArray): Pair<Transport, String>? {
        synchronized(peersByAddress) {
            for ((address, peer) in peersByAddress) {
                val known = peer.userId ?: continue
                if (known.contentEquals(userId)) return peer.transport to address
            }
            return null
        }
    }

    /** Forget every connection, e.g. when the mesh service stops and all links die with it. */
    fun clear() {
        synchronized(peersByAddress) {
            peersByAddress.clear()
        }
    }
}
