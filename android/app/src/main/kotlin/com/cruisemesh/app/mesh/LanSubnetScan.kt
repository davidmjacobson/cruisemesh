package com.cruisemesh.app.mesh

import java.net.Inet4Address
import java.net.InetAddress

// A cruise-ship LAN is commonly a single flat /16 (or bigger), so the
// user-initiated "Search my local network" action follows the network's own
// advertised prefix -- but never broader than a /16 (65,534 hosts), which
// bounds it to something a phone can finish in the background. A home /24
// still scans just its 254 hosts; only genuinely huge networks (e.g. a /8)
// get clamped down to a /16 around this phone's address. This ceiling is for
// the manual button only, explicitly allowed to be big since the user asked.
internal const val BROADEST_SCAN_PREFIX_LENGTH = 16

// Ceiling for the *automatic* full-subnet fallback sweep: a /16 is up to
// ~65k TCP probes, minutes of sustained radio at concurrency 64. A /20 is
// ~4,094 hosts, a much smaller unattended background cost. Ship/hotel Wi-Fi
// is exactly where the underlying network is a huge flat subnet, so the
// automatic path stays narrower than the manual one.
internal const val BROADEST_AUTOMATIC_SCAN_PREFIX_LENGTH = 20

// /31 and /32 have no usable host addresses and /30 already yields only two;
// clamp here so [subnetHosts] always has at least a couple of candidates.
internal const val NARROWEST_SCAN_PREFIX_LENGTH = 30

// Used when the network doesn't report a prefix length for our address.
internal const val DEFAULT_SCAN_PREFIX_LENGTH = 24

/** The prefix length [subnetHosts] actually scans for a network-reported [prefixLength]. */
internal fun effectiveScanPrefixLength(prefixLength: Int): Int =
    prefixLength.coerceIn(BROADEST_SCAN_PREFIX_LENGTH, NARROWEST_SCAN_PREFIX_LENGTH)

/**
 * Same clamp as [effectiveScanPrefixLength], but for the automatic
 * full-subnet sweep, which is capped narrower (see
 * [BROADEST_AUTOMATIC_SCAN_PREFIX_LENGTH]).
 */
internal fun effectiveAutomaticScanPrefixLength(prefixLength: Int): Int =
    prefixLength.coerceIn(BROADEST_AUTOMATIC_SCAN_PREFIX_LENGTH, NARROWEST_SCAN_PREFIX_LENGTH)

/**
 * Candidate hosts in the IPv4 subnet [localAddress] sits in, as defined by
 * [prefixLength] (clamped by [effectiveScanPrefixLength]), excluding this
 * phone plus the network and broadcast addresses.
 */
internal fun subnetHosts(localAddress: Inet4Address, prefixLength: Int): List<InetAddress> {
    val effectivePrefix = effectiveScanPrefixLength(prefixLength)
    val localValue = localAddress.address.toIpv4Long()
    // effectivePrefix is 16..30, so the shift is always in range and the mask
    // never spans the full 32 bits (which would need special-casing).
    val mask = (0xFFFF_FFFFL shl (32 - effectivePrefix)) and 0xFFFF_FFFFL
    val network = localValue and mask
    val broadcast = network or (mask.inv() and 0xFFFF_FFFFL)
    val hosts = ArrayList<InetAddress>(((broadcast - network - 1).coerceAtLeast(0)).toInt())
    var host = network + 1
    while (host < broadcast) {
        if (host != localValue) {
            hosts.add(InetAddress.getByAddress(host.toIpv4Bytes()))
        }
        host++
    }
    return hosts
}

private fun ByteArray.toIpv4Long(): Long =
    fold(0L) { acc, byte -> (acc shl 8) or (byte.toLong() and 0xFF) }

private fun Long.toIpv4Bytes(): ByteArray = byteArrayOf(
    ((this shr 24) and 0xFF).toByte(),
    ((this shr 16) and 0xFF).toByte(),
    ((this shr 8) and 0xFF).toByte(),
    (this and 0xFF).toByte(),
)
