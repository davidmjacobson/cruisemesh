package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreLanEndpoint
import uniffi.cruisemesh_core.coreMakeLanEndpointLink
import uniffi.cruisemesh_core.coreParseLanEndpointLink
import uniffi.cruisemesh_core.lanHostsShareLocalNetwork

internal fun lanEndpointLink(endpoint: LanManualEndpoint): String {
    return coreMakeLanEndpointLink(CoreLanEndpoint(endpoint.host, endpoint.port.toUShort()))
}

internal fun parseLanEndpointLink(fragment: String?): LanManualEndpoint? {
    val endpoint = coreParseLanEndpointLink(fragment) ?: return null
    return LanManualEndpoint(endpoint.host, endpoint.port.toInt())
}

/**
 * What a tapped or scanned LAN address link may do, decided before anything
 * is dialed.
 *
 * A link is someone else's input. Dialing it unasked would hand whoever wrote
 * it this phone's address and a handshake attempt, and turning the mesh on
 * for it would override the person's own choice to leave it off. So a link
 * never starts the mesh, never dials an address that is not on the Wi-Fi
 * network this phone is on, and never dials anything without the person
 * saying yes to the address it names. Only [Confirm] leads to a connection,
 * and only after that yes.
 *
 * A link is the only way a manual address reaches the transport, so this is
 * the whole gate. The cost is that a link to a peer on a routed, different
 * subnet is refused; such a peer is still reached through the contact's own
 * hint once both phones are friends.
 */
internal sealed interface LanEndpointLinkDecision {
    val endpoint: LanManualEndpoint

    /** The mesh is off. The link names the address but turns nothing on. */
    data class MeshOff(override val endpoint: LanManualEndpoint) : LanEndpointLinkDecision

    /** This phone has no Wi-Fi address to compare the link against. */
    data class NoWifi(override val endpoint: LanManualEndpoint) : LanEndpointLinkDecision

    /** The address is not on the Wi-Fi network this phone is using. */
    data class OtherNetwork(override val endpoint: LanManualEndpoint) : LanEndpointLinkDecision

    /** Show the address and connect only if the person agrees. */
    data class Confirm(override val endpoint: LanManualEndpoint) : LanEndpointLinkDecision
}

/**
 * Decide what [endpoint] from a link may do. [localEndpoint] is this phone's
 * own published LAN endpoint (`host:port`, IPv6 bracketed), or null when it
 * has none. The same-network test is the shared core's
 * `lanHostsShareLocalNetwork`; [sameNetwork] is only a seam for tests.
 */
internal fun decideLanEndpointLink(
    endpoint: LanManualEndpoint,
    meshRunning: Boolean,
    localEndpoint: String?,
    sameNetwork: (localHost: String, candidateHost: String) -> Boolean = { local, candidate ->
        lanHostsShareLocalNetwork(localHost = local, candidateHost = candidate)
    },
): LanEndpointLinkDecision {
    if (!meshRunning) return LanEndpointLinkDecision.MeshOff(endpoint)
    // The port is irrelevant to the comparison; any valid one parses the host.
    val localHost = localEndpoint?.let { parseLanManualEndpoint(it, defaultPort = 1)?.host }
        ?: return LanEndpointLinkDecision.NoWifi(endpoint)
    if (!sameNetwork(localHost, endpoint.host)) return LanEndpointLinkDecision.OtherNetwork(endpoint)
    return LanEndpointLinkDecision.Confirm(endpoint)
}
