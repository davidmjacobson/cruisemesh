package com.cruisemesh.app.mesh

import java.net.Inet4Address
import java.net.InetAddress

/** Candidate hosts in the local IPv4 /24, excluding this phone and .0/.255. */
internal fun subnet24Hosts(localAddress: Inet4Address): List<InetAddress> {
    val octets = localAddress.address
    return (1..254)
        .asSequence()
        .filter { it.toByte() != octets[3] }
        .map { last ->
            InetAddress.getByAddress(
                byteArrayOf(octets[0], octets[1], octets[2], last.toByte()),
            )
        }
        .toList()
}
