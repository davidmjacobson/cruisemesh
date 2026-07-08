package com.cruisemesh.app.mesh

import android.util.Log

private const val TAG = "MeshRouter"

/**
 * Process-wide singleton (same lazy/eager-object pattern as
 * [com.cruisemesh.app.AppStore]) that owns the live "send a frame to this
 * peer" operation for the whole app, backed by the pure [MeshRouterState]
 * mapping. [MeshService] registers its two transports' send functions on
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
 *    used by [MeshService] for HELLO-triggered replay and delivery receipts
 *    (DESIGN.md §7.2, §7.3 interim), where correctness means answering the
 *    same connection, not just "any" connection to that userId.
 */
object MeshRouter {
    private val state = MeshRouterState()

    @Volatile private var centralSend: ((String, ByteArray) -> Unit)? = null
    @Volatile private var peripheralSend: ((String, ByteArray) -> Unit)? = null

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

    /** A link to [address] over [transport] just became usable; see [MeshRouterState]. */
    fun onConnected(address: String, transport: MeshRouterState.Transport) = state.onConnected(address, transport)

    /** [address] disconnected; forget its mapping so nothing sends to a dead link. */
    fun onDisconnected(address: String) = state.onDisconnected(address)

    /** [address] identified itself as [userId] via a HELLO frame. */
    fun onHello(address: String, userId: ByteArray) = state.onHello(address, userId)

    /** The userId [address] identified as, if known. */
    fun userIdFor(address: String): ByteArray? = state.userIdFor(address)

    /**
     * Sends [frame] to whichever live link has identified itself as [userId].
     * Returns false if no connected link currently maps to that userId --
     * callers treat that as "stays local for now"; HELLO-triggered replay
     * (DESIGN.md §7.3 interim) delivers it once the peer is next seen.
     */
    fun sendToUserId(userId: ByteArray, frame: ByteArray): Boolean {
        val (transport, address) = state.routeFor(userId) ?: return false
        return dispatch(transport, address, frame)
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

    private fun dispatch(transport: MeshRouterState.Transport, address: String, frame: ByteArray): Boolean {
        val send = when (transport) {
            MeshRouterState.Transport.CENTRAL -> centralSend
            MeshRouterState.Transport.PERIPHERAL -> peripheralSend
        }
        if (send == null) {
            Log.w(TAG, "dispatch: no live $transport transport registered; dropping send to $address")
            return false
        }
        send(address, frame)
        return true
    }
}
