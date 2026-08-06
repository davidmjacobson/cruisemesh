package com.cruisemesh.app.mesh

import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.CoreCarriedCursor
import uniffi.cruisemesh_core.CoreCarriedLane
import uniffi.cruisemesh_core.CoreMeshRouterState
import uniffi.cruisemesh_core.CoreTransport

/** Android-shaped adapter around the shared, thread-safe Rust route state. */
class MeshRouterState {
    enum class Transport(val routePriority: Int) { CENTRAL(0), PERIPHERAL(0), LAN(10) }
    data class IdentifiedRoute(val transport: Transport, val address: String, val userId: ByteArray)

    private val core = CoreMeshRouterState()

    fun onConnected(address: String, transport: Transport) = core.onConnected(address, transport.toCore())
    fun onDisconnected(address: String) = core.onDisconnected(address)
    fun onHello(address: String, userId: ByteArray): Boolean = core.onHello(address, userId)
    fun onHello2(address: String, userId: ByteArray, capabilities: UInt): Boolean =
        core.onHello2(address, userId, capabilities)
    fun peerAcksHiddenKinds(address: String): Boolean = core.peerAcksHiddenKinds(address)
    fun hiddenOfferedFor(address: String): List<ByteArray> = core.hiddenOfferedFor(address)
    fun recordHiddenOffered(address: String, msgIds: List<ByteArray>) = core.recordHiddenOffered(address, msgIds)
    fun carriedLaneFor(address: String, nowMs: Long): CoreCarriedLane = core.carriedLaneFor(address, nowMs)
    fun recordCarriedProgress(address: String, next: CoreCarriedCursor?, exhausted: Boolean, nowMs: Long) =
        core.recordCarriedProgress(address, next, exhausted, nowMs)
    fun targetedCarriedLaneFor(address: String, nowMs: Long): CoreCarriedLane =
        core.targetedCarriedLaneFor(address, nowMs)
    fun recordTargetedCarriedProgress(address: String, next: CoreCarriedCursor?, exhausted: Boolean, nowMs: Long) =
        core.recordTargetedCarriedProgress(address, next, exhausted, nowMs)
    fun userIdFor(address: String): ByteArray? = core.userIdFor(address)
    fun transportFor(address: String): Transport? = core.transportFor(address)?.toPlatform()
    fun connectedRoutes(): List<Pair<Transport, String>> = core.connectedRoutes().map { it.transport.toPlatform() to it.address }
    fun identifiedRoutes(): List<IdentifiedRoute> = core.identifiedRoutes().map { IdentifiedRoute(it.transport.toPlatform(), it.address, it.userId) }
    fun routeFor(userId: ByteArray): Pair<Transport, String>? = core.routeFor(userId)?.let { it.transport.toPlatform() to it.address }
    fun routesFor(userId: ByteArray): List<Pair<Transport, String>> = core.routesFor(userId).map { it.transport.toPlatform() to it.address }
    fun helloedUserIds(): Set<String> = core.helloedUserIds().mapTo(mutableSetOf(), UserIdHex::encode)

    /**
     * One flood route per authenticated logical peer. Android can hold both
     * BLE roles, a LAN socket, and rotating addresses for one phone at once;
     * treating those as independent epidemic peers multiplies every foreign,
     * profile, and friend-directory frame. Links that have not HELLO'd remain
     * independent because there is no safe identity with which to collapse
     * them. If [exceptAddress] is identified, every route for that same user
     * is excluded so a frame is not echoed straight back over its other role.
     */
    fun relayRoutes(exceptAddress: String? = null): List<Pair<Transport, String>> =
        logicalRelayPlan(connectedRoutes(), identifiedRoutes(), exceptAddress)

    /**
     * hex userId -> the transport a send to that userId would take right now
     * (its highest-[Transport.routePriority] HELLO'd link, so LAN wins over
     * BLE -- the same precedence as [routeFor]). One entry per
     * [helloedUserIds] member. Exposed so the connectivity flow can publish an
     * *observable* per-contact transport: [routeFor] read imperatively from a
     * composable only re-samples when its inputs change, so a LAN->BLE handoff
     * that leaves the userId still HELLO'd (Wi-Fi dropped, BLE link survives)
     * never flipped the "Nearby via Wi-Fi/Bluetooth" copy. This map changing is
     * what makes that flip recompose.
     */
    fun nearbyTransports(): Map<String, Transport> {
        val best = HashMap<String, Transport>()
        for (route in identifiedRoutes()) {
            val hex = UserIdHex.encode(route.userId)
            val current = best[hex]
            if (current == null || route.transport.routePriority > current.routePriority) {
                best[hex] = route.transport
            }
        }
        return best
    }

    fun clearTransports(transports: Set<Transport>) = core.clearTransports(transports.map(Transport::toCore))
    fun clear() = core.clear()
}

internal fun logicalRelayPlan(
    connected: List<Pair<MeshRouterState.Transport, String>>,
    identified: List<MeshRouterState.IdentifiedRoute>,
    exceptAddress: String?,
): List<Pair<MeshRouterState.Transport, String>> {
    val userByAddress = identified.associate { it.address to UserIdHex.encode(it.userId) }
    val excludedUser = exceptAddress?.let(userByAddress::get)
    val selected = mutableListOf<Pair<MeshRouterState.Transport, String>>()

    for ((userId, routes) in connected.groupBy { (_, address) -> userByAddress[address] }) {
        if (userId == null) {
            selected += routes.filterNot { (_, address) -> address == exceptAddress }
            continue
        }
        if (userId == excludedUser) continue
        selected += routes.sortedWith(
            compareByDescending<Pair<MeshRouterState.Transport, String>> { (transport, _) ->
                transport.routePriority
            }.thenBy { (_, address) -> address },
        ).first()
    }
    return selected
}

internal fun MeshRouterState.Transport.toCore(): CoreTransport = when (this) {
    MeshRouterState.Transport.CENTRAL -> CoreTransport.CENTRAL
    MeshRouterState.Transport.PERIPHERAL -> CoreTransport.PERIPHERAL
    MeshRouterState.Transport.LAN -> CoreTransport.LAN
}

internal fun CoreTransport.toPlatform(): MeshRouterState.Transport = when (this) {
    CoreTransport.CENTRAL -> MeshRouterState.Transport.CENTRAL
    CoreTransport.PERIPHERAL -> MeshRouterState.Transport.PERIPHERAL
    CoreTransport.LAN -> MeshRouterState.Transport.LAN
}
