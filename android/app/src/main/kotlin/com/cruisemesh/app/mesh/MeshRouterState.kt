package com.cruisemesh.app.mesh

import com.cruisemesh.app.chat.UserIdHex
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
    fun userIdFor(address: String): ByteArray? = core.userIdFor(address)
    fun transportFor(address: String): Transport? = core.transportFor(address)?.toPlatform()
    fun connectedRoutes(): List<Pair<Transport, String>> = core.connectedRoutes().map { it.transport.toPlatform() to it.address }
    fun identifiedRoutes(): List<IdentifiedRoute> = core.identifiedRoutes().map { IdentifiedRoute(it.transport.toPlatform(), it.address, it.userId) }
    fun routeFor(userId: ByteArray): Pair<Transport, String>? = core.routeFor(userId)?.let { it.transport.toPlatform() to it.address }
    fun routesFor(userId: ByteArray): List<Pair<Transport, String>> = core.routesFor(userId).map { it.transport.toPlatform() to it.address }
    fun helloedUserIds(): Set<String> = core.helloedUserIds().mapTo(mutableSetOf(), UserIdHex::encode)

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
