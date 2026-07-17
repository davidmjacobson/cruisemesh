package com.cruisemesh.app.mesh

import android.util.Log

private const val TAG = "MeshRouter"

/**
 * Process-wide singleton (same lazy/eager-object pattern as
 * [com.cruisemesh.app.AppStore]) that owns the live "send a frame to this
 * peer" operation for the whole app, backed by the pure [MeshRouterState]
 * mapping. [MeshService] registers the BLE-role and LAN send functions on
 * start and unregisters them on stop; [com.cruisemesh.app.chat.MeshSender]
 * implementations call [sendToUserId] without ever needing to know a BLE
 * address, a role, or whether MeshService is even running.
 *
 * There are two ways to address a send, matching the two things callers
 * actually know:
 *  - [sendToUserId]: "get this to contact C" -- used by the outgoing chat
 *    send path, which only has a [uniffi.cruisemesh_core.Contact], not an
 *    address.
 *  - [sendToAddress]: "reply on the exact link this frame arrived on" --
 *    used by [MeshService] for HELLO/DIGEST exchange and delivery/read
 *    receipts (DESIGN.md §7.2, §7.3), where correctness means answering the
 *    same connection, not just "any" connection to that userId.
 */
object MeshRouter {
    private val state = MeshRouterState()

    @Volatile private var centralSend: ((String, ByteArray) -> Unit)? = null
    @Volatile private var peripheralSend: ((String, ByteArray) -> Unit)? = null
    @Volatile private var lanSend: ((String, ByteArray) -> Unit)? = null

    /** [MeshService] calls these when its BLE roles start. */
    fun registerCentral(send: (String, ByteArray) -> Unit) {
        centralSend = send
    }

    fun registerPeripheral(send: (String, ByteArray) -> Unit) {
        peripheralSend = send
    }

    /** [MeshService] calls these when its BLE roles stop, so a stale send function is never invoked. */
    fun unregisterCentral() {
        centralSend = null
    }

    fun unregisterPeripheral() {
        peripheralSend = null
    }

    fun registerLan(send: (String, ByteArray) -> Unit) {
        lanSend = send
    }

    fun unregisterLan() {
        lanSend = null
    }

    /**
     * Drops all address mappings. [MeshService] calls this on stop: its BLE
     * roles' stop() paths tear down connections without firing per-address
     * disconnect callbacks, so without this a stop/start of the mesh within
     * one process would leave stale addresses that [sendToAddress] would
     * happily (and uselessly) target.
     */
    fun reset() {
        state.clear()
    }

    /** BLE teardown must not discard authenticated same-LAN routes. */
    fun resetBle() {
        state.clearTransports(
            setOf(
                MeshRouterState.Transport.CENTRAL,
                MeshRouterState.Transport.PERIPHERAL,
            ),
        )
    }

    /** A link to [address] over [transport] just became usable; see [MeshRouterState]. */
    fun onConnected(address: String, transport: MeshRouterState.Transport) = state.onConnected(address, transport)

    /** [address] disconnected; forget its mapping so nothing sends to a dead link. */
    fun onDisconnected(address: String) = state.onDisconnected(address)

    /** [address] identified itself as [userId] via a HELLO frame. */
    fun onHello(address: String, userId: ByteArray): Boolean = state.onHello(address, userId)

    /** The userId [address] identified as, if known. */
    fun userIdFor(address: String): ByteArray? = state.userIdFor(address)

    /** The live transport backing [address], if it is still connected. */
    fun transportFor(address: String): MeshRouterState.Transport? = state.transportFor(address)

    /** Distinct HELLO'd peer userIds, hex-encoded; see [MeshRouterState.helloedUserIds]. */
    fun helloedUserIds(): Set<String> = state.helloedUserIds()

    /** Live routes that have identified themselves via HELLO. */
    fun identifiedRoutes(): List<MeshRouterState.IdentifiedRoute> = state.identifiedRoutes()

    /**
     * Sends [frame] to whichever live link has identified itself as [userId].
     * Returns false if no connected link currently maps to that userId --
     * callers treat that as "stays local for now"; the digest sync
     * (DESIGN.md §7.3) delivers it once the peer is next seen and HELLOs in.
     */
    fun sendToUserId(userId: ByteArray, frame: ByteArray): Boolean {
        val routes = state.routesFor(userId)
        if (routes.isEmpty()) return false
        val selected = transportSendPlan(routes, frame.size)
        var sent = false
        for ((transport, address) in selected) {
            sent = dispatch(transport, address, frame) || sent
        }
        return sent
    }

    /**
     * Sends [frame] on the exact link [address] refers to, regardless of
     * whether its userId is known yet (a HELLO reply target is, by
     * construction, always a userId-less address at send time).
     */
    fun sendToAddress(address: String, frame: ByteArray): Boolean {
        val transport = state.transportFor(address) ?: run {
            Log.w(TAG, "sendToAddress: $address is not currently tracked as connected")
            return false
        }
        return dispatch(transport, address, frame)
    }

    /**
     * Floods [frame] to every currently connected link except [exceptAddress]
     * (the one it arrived on) -- the epidemic-relay send primitive for
     * DESIGN.md §5.3 gossip. Returns the number of links it was dispatched to.
     * Callers are responsible for dedupe (see [com.cruisemesh.app.mesh.GossipState])
     * and hop-budget checks before relaying; this method just sprays the frame
     * outward. Excluding the arriving link avoids the trivial immediate
     * echo-back; the seen-ID set and `hop_ttl` bound the rest of the flood.
     */
    fun relayToAllExcept(exceptAddress: String, frame: ByteArray): Int {
        var sent = 0
        for ((transport, address) in state.connectedRoutes()) {
            if (address == exceptAddress) continue
            if (dispatch(transport, address, frame)) sent++
        }
        return sent
    }

    /** Floods [frame] to every currently connected link. */
    fun relayToAll(frame: ByteArray): Int {
        var sent = 0
        for ((transport, address) in state.connectedRoutes()) {
            if (dispatch(transport, address, frame)) sent++
        }
        return sent
    }

    private fun dispatch(transport: MeshRouterState.Transport, address: String, frame: ByteArray): Boolean {
        val send = when (transport) {
            MeshRouterState.Transport.CENTRAL -> centralSend
            MeshRouterState.Transport.PERIPHERAL -> peripheralSend
            MeshRouterState.Transport.LAN -> lanSend
        }
        if (send == null) {
            Log.w(TAG, "dispatch: no live $transport transport registered; dropping send to $address")
            return false
        }
        send(address, frame)
        return true
    }
}

/**
 * Small control/text frames race over LAN plus one BLE route; large payloads
 * use only the highest-priority route so photos do not duplicate over BLE.
 */
internal fun transportSendPlan(
    routes: List<Pair<MeshRouterState.Transport, String>>,
    frameSize: Int,
): List<Pair<MeshRouterState.Transport, String>> =
    uniffi.cruisemesh_core.coreTransportSendPlan(
        routes.map { uniffi.cruisemesh_core.CoreTransportRoute(it.first.toCore(), it.second) },
        frameSize.toUInt(),
    ).map { it.transport.toPlatform() to it.address }
