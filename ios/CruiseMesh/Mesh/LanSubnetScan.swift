import Foundation

/// Ceiling breadth for the user-initiated "Search local subnet" action --
/// explicitly allowed to be big since the user asked for it.
let broadestLanScanPrefixLength = 16
/// Ceiling breadth for the *automatic* full-subnet fallback sweep. A /16 is
/// ~65k hosts, minutes of sustained radio at concurrency 64; a /20 is
/// ~4,094 hosts, a much smaller background cost. Ship/hotel Wi-Fi is
/// exactly where the underlying network is a huge flat subnet, so the
/// unattended path stays narrower than the manual one.
let broadestAutomaticLanScanPrefixLength = 20
let narrowestLanScanPrefixLength = 30
let defaultLanScanPrefixLength = 24

struct LocalWifiIPv4Network: Equatable {
    let address: String
    let prefixLength: Int
}

func effectiveLanScanPrefixLength(_ prefixLength: Int) -> Int {
    min(max(prefixLength, broadestLanScanPrefixLength), narrowestLanScanPrefixLength)
}

/// Same clamp as `effectiveLanScanPrefixLength`, but for the automatic
/// full-subnet sweep, which is capped narrower (see
/// `broadestAutomaticLanScanPrefixLength`).
func effectiveAutomaticLanScanPrefixLength(_ prefixLength: Int) -> Int {
    min(max(prefixLength, broadestAutomaticLanScanPrefixLength), narrowestLanScanPrefixLength)
}

/// Candidate hosts in the selected IPv4 subnet, excluding this phone plus the
/// network and broadcast addresses. Breadth is always clamped to /16.../30.
func lanSubnetHosts(localAddress: String, prefixLength: Int) -> [String] {
    guard let local = ipv4Value(localAddress) else { return [] }
    let effectivePrefix = effectiveLanScanPrefixLength(prefixLength)
    let mask = UInt32.max << UInt32(32 - effectivePrefix)
    let network = local & mask
    let broadcast = network | ~mask
    guard network < broadcast else { return [] }

    var hosts: [String] = []
    hosts.reserveCapacity(max(Int(broadcast - network - 2), 0))
    var host = network + 1
    while host < broadcast {
        if host != local {
            hosts.append(ipv4String(host))
        }
        host += 1
    }
    return hosts
}

func ipv4PrefixLength(netmask: UInt32) -> Int? {
    let inverse = ~netmask
    guard inverse & (inverse &+ 1) == 0 else { return nil }
    return netmask.nonzeroBitCount
}

private func ipv4Value(_ address: String) -> UInt32? {
    let parts = address.split(separator: ".", omittingEmptySubsequences: false)
    guard parts.count == 4 else { return nil }
    var value: UInt32 = 0
    for part in parts {
        guard let octet = UInt8(part) else { return nil }
        value = (value << 8) | UInt32(octet)
    }
    return value
}

private func ipv4String(_ value: UInt32) -> String {
    [24, 16, 8, 0]
        .map { String((value >> UInt32($0)) & 0xff) }
        .joined(separator: ".")
}
