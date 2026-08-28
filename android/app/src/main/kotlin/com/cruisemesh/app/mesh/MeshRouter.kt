package com.cruisemesh.app.mesh

import android.util.Log
import uniffi.cruisemesh_core.CoreCarriedCursor
import uniffi.cruisemesh_core.CoreCarriedLane

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

    /**
     * The shared Rust route state, for `MessageStore.corePlanMeshMeet` alone
     * (see [MeshRouterState.coreState]). Everything else goes through the
     * named delegations below.
     */
    internal val coreState get() = state.coreState

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

    /**
     * LAN teardown must not discard BLE routes.
     *
     * The mirror of [resetBle], needed for the same reason: closing LAN sockets
     * in bulk does not fire per-address disconnect callbacks, so without this a
     * §9.4 silence window (or any other LAN restart) leaves stale LAN addresses
     * that [sendToAddress] would happily target -- a phone that believes it can
     * still reach peers over a transport it has just taken down.
     */
    fun resetLan() {
        state.clearTransports(setOf(MeshRouterState.Transport.LAN))
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

    /** Identity used by Rust to elect the same BLE direction at both peers. */
    fun setLocalUserId(userId: ByteArray) = state.setLocalUserId(userId)

    /** A link to [address] over [transport] just became usable; see [MeshRouterState]. */
    fun onConnected(address: String, transport: MeshRouterState.Transport) = state.onConnected(address, transport)

    /**
     * A link that proved it belongs to one of *this person's own* devices
     * (`specs/multi-device-v1.md` §10 step 5): a transport for the roster
     * notice, and never a route.
     *
     * Distinct from [onConnected] because "no user id yet" and "never a peer"
     * are different facts that look alike from here. Core floods every link of
     * the first kind -- that is what makes gossip work before a HELLO lands --
     * and the device still holding this person's agreement key after a removal
     * is the device that was removed.
     */
    fun onOwnDeviceConnected(address: String, transport: MeshRouterState.Transport) =
        state.onOwnDeviceConnected(address, transport)

    /** [address] disconnected; forget its mapping so nothing sends to a dead link. */
    fun onDisconnected(address: String) = state.onDisconnected(address)

    /** [address] identified itself as [userId] via a HELLO frame. */
    fun onHello(address: String, userId: ByteArray): Boolean = state.onHello(address, userId)

    /** [address]'s HELLO2 follow-up: identity + capability bits. */
    fun onHello2(address: String, userId: ByteArray, capabilities: UInt): Boolean =
        state.onHello2(address, userId, capabilities)

    /**
     * Whether [address] advertised the capability bit for this one hidden
     * spray [kind] -- asked per kind, so a peer that acks four of the five it
     * knows about keeps the watermark for those four. False for a pre-HELLO2
     * build, which advertises nothing.
     */
    fun peerAcksHiddenKind(address: String, kind: UByte): Boolean =
        state.peerAcksHiddenKind(address, kind)

    /** Every hidden spray kind [address] will ack, for the plan builder. */
    fun peerAckedHiddenKinds(address: String): ByteArray = state.peerAckedHiddenKinds(address)

    /** Hidden-kind msg_ids already sprayed to [address] this link session. */
    fun hiddenOfferedFor(address: String): List<ByteArray> = state.hiddenOfferedFor(address)

    fun recordHiddenOffered(address: String, msgIds: List<ByteArray>) =
        state.recordHiddenOffered(address, msgIds)

    /**
     * Where this link's foreign-carry lane should resume, or whether to sit
     * this re-digest out because the walk is done and still cooling down.
     */
    fun carriedLaneFor(address: String, nowMs: Long): CoreCarriedLane =
        state.carriedLaneFor(address, nowMs)

    /** Record how far the carried lane just walked down [address]. */
    fun recordCarriedProgress(address: String, next: CoreCarriedCursor?, exhausted: Boolean, nowMs: Long) =
        state.recordCarriedProgress(address, next, exhausted, nowMs)

    /** Targeted HELLO drain lane (envelopes for this peer) — G2. */
    fun targetedCarriedLaneFor(address: String, nowMs: Long): CoreCarriedLane =
        state.targetedCarriedLaneFor(address, nowMs)

    fun recordTargetedCarriedProgress(address: String, next: CoreCarriedCursor?, exhausted: Boolean, nowMs: Long) =
        state.recordTargetedCarriedProgress(address, next, exhausted, nowMs)

    /** The userId [address] identified as, if known. */
    fun userIdFor(address: String): ByteArray? = state.userIdFor(address)

    /** The live transport backing [address], if it is still connected. */
    fun transportFor(address: String): MeshRouterState.Transport? = state.transportFor(address)

    /**
     * The route Rust elects for a send to [userId] right now (LAN wins over
     * BLE) -- e.g. so UI copy can say which transport a
     * [ReachabilityLevel.NEARBY] contact is actually nearby over instead of
     * assuming BLE.
     */
    fun routeFor(userId: ByteArray): Pair<MeshRouterState.Transport, String>? = state.routeFor(userId)

    /** Distinct HELLO'd peer userIds, hex-encoded; see [MeshRouterState.helloedUserIds]. */
    fun helloedUserIds(): Set<String> = state.helloedUserIds()

    /** Number of authenticated logical peers, independent of physical links. */
    fun connectedUserCount(): Int = state.connectedUserCount()

    /** Per-userId live transport for every HELLO'd peer; see [MeshRouterState.nearbyTransports]. */
    fun nearbyTransports(): Map<String, MeshRouterState.Transport> = state.nearbyTransports()

    /** Live routes that have identified themselves via HELLO. */
    fun identifiedRoutes(): List<MeshRouterState.IdentifiedRoute> = state.identifiedRoutes()

    /**
     * Live links to a device of this person's own (`specs/multi-device-v1.md`
     * §10 step 5). Never routes, so never in [identifiedRoutes] -- and that is
     * exactly why they have to be nameable: something has to heartbeat them
     * and re-offer the roster on them.
     */
    fun ownDeviceLinks(): List<Pair<MeshRouterState.Transport, String>> = state.ownDeviceLinks()

    /** One elected application-data route per authenticated logical peer. */
    fun selectedIdentifiedRoutes(): List<MeshRouterState.IdentifiedRoute> = state.selectedIdentifiedRoutes()

    /** False for a superseded link retained only for exact-link control traffic. */
    fun isSelectedRoute(address: String): Boolean = state.isSelectedRoute(address)

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
     * Floods [frame] to one route per logical peer except [exceptAddress]'s
     * peer (the one it arrived from) -- the epidemic-relay send primitive for
     * DESIGN.md §5.3 gossip. Returns the number of links it was dispatched to.
     * Callers are responsible for dedupe (see [com.cruisemesh.app.mesh.GossipState])
     * and hop-budget checks before relaying; this method just sprays the frame
     * outward. Unidentified links remain independent until HELLO gives us an
     * identity to collapse. Excluding all routes for the arriving logical
     * peer avoids echoing the frame back over that phone's other BLE role.
     */
    fun relayToAllExcept(exceptAddress: String, frame: ByteArray): Int {
        var sent = 0
        for ((transport, address) in state.relayRoutes(exceptAddress)) {
            if (dispatch(transport, address, frame)) sent++
        }
        return sent
    }

    /** Floods [frame] once to each authenticated peer (plus each unknown link). */
    fun relayToAll(frame: ByteArray): Int {
        var sent = 0
        for ((transport, address) in state.relayRoutes()) {
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
 * Returns the first route from Rust's preference-ordered [MeshRouterState.routesFor]
 * result. Do not pass an arbitrary route list: BLE role preference depends on
 * both authenticated user IDs and cannot be reconstructed from transport type
 * alone. When the elected route disconnects, the next router query naturally
 * promotes the best remaining route.
 */
internal fun transportSendPlan(
    routes: List<Pair<MeshRouterState.Transport, String>>,
    frameSize: Int,
): List<Pair<MeshRouterState.Transport, String>> =
    uniffi.cruisemesh_core.coreTransportSendPlan(
        routes.map { uniffi.cruisemesh_core.CoreTransportRoute(it.first.toCore(), it.second) },
        frameSize.toUInt(),
    ).map { it.transport.toPlatform() to it.address }
