import Darwin

struct LanManualEndpoint: Codable, Equatable {
    let host: String
    let port: UInt16

    var display: String {
        coreFormatLanEndpoint(endpoint: CoreLanEndpoint(host: host, port: port))
    }
}

func parseLanManualEndpoint(_ text: String, defaultPort: UInt16 = lanDefaultTcpPort()) -> LanManualEndpoint? {
    guard let endpoint = coreParseLanEndpoint(text: text, defaultPort: defaultPort) else { return nil }
    return LanManualEndpoint(host: endpoint.host, port: endpoint.port)
}

func lanEndpointLink(_ endpoint: LanManualEndpoint) -> String {
    coreMakeLanEndpointLink(endpoint: CoreLanEndpoint(host: endpoint.host, port: endpoint.port))
}

func parseLanEndpointLink(_ fragment: String?) -> LanManualEndpoint? {
    guard let endpoint = coreParseLanEndpointLink(fragment: fragment) else { return nil }
    return LanManualEndpoint(host: endpoint.host, port: endpoint.port)
}

/// Prefix marking a connection key that came from a contact's LAN hint.
let lanHintKeyPrefix = "hint:"

/// Prefix marking a connection key replayed from the saved endpoint cache.
let lanCachedKeyPrefix = "cache:"

func lanHintConnectKey(_ endpointDisplay: String) -> String {
    "\(lanHintKeyPrefix)\(endpointDisplay)"
}

func lanCachedConnectKey(_ endpointDisplay: String) -> String {
    "\(lanCachedKeyPrefix)\(endpointDisplay)"
}

/// Whether a connection key may only ever be attempted once per piece of
/// evidence. Keys this phone found itself -- Bonjour service names, subnet
/// sweep hits, a manual address a human typed -- keep retrying on a timer.
/// Two kinds do not:
///
/// - a hint carries an address supplied by the contact rather than one this
///   phone observed, so it is tried when it arrives and never retried;
/// - a cached endpoint is a *remembered* hint, so it is no better evidence
///   than the hint was. Retrying it on a timer turned a single stale address
///   into a dial every sixty seconds for as long as the phone stayed on the
///   network.
///
/// Retry coverage is not lost. `MeshController`'s `onNetworkReady` replays
/// every cached endpoint on each Wi-Fi join, so a cached address still gets
/// one attempt per network join, plus another whenever a fresh hint or
/// Bonjour discovery arrives -- the only kind of event that can make a dead
/// address live again. And single-shot is only the state an *unproven*
/// address is in: once one of these completes a Noise handshake,
/// `LanTransport.connectionAuthenticated` files it as a retry endpoint like
/// any other proven link, so a dropped link still comes back on the timer.
/// The endpoint is retired the moment an attempt fails again.
func isSingleShotLanConnectKey(_ serviceKey: String) -> Bool {
    serviceKey.hasPrefix(lanHintKeyPrefix) || serviceKey.hasPrefix(lanCachedKeyPrefix)
}

/// Whether a hinted host may be *filed* in this phone's endpoint cache, given
/// the phone's own LAN address. Dialing a hint across subnets is deliberate;
/// remembering it as if it belonged to the network we are on is the defect
/// this closes -- the shared core is the authority for the comparison.
///
/// No comparable local address means no filing. Nothing is lost by that here:
/// this phone's cache is keyed by an IPv4 network fingerprint
/// (`lanNetworkId(ipv4Address:)`), so without an IPv4 address there is no
/// cache to write to in the first place. And if the hint's dial
/// authenticates, `onLanPeerAuthenticated` files the endpoint anyway, on the
/// stronger authority of having reached it.
func lanHintMayBeCached(localHost: String?, candidateHost: String) -> Bool {
    guard let localHost else { return false }
    return lanHostsShareLocalNetwork(localHost: localHost, candidateHost: candidateHost)
}

/// The active Wi-Fi IPv4 address and its advertised subnet prefix.
func localWifiIPv4Network() -> LocalWifiIPv4Network? {
    var firstAddress: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&firstAddress) == 0, let firstAddress else { return nil }
    defer { freeifaddrs(firstAddress) }

    var cursor: UnsafeMutablePointer<ifaddrs>? = firstAddress
    while let current = cursor {
        defer { cursor = current.pointee.ifa_next }
        guard let address = current.pointee.ifa_addr,
              address.pointee.sa_family == UInt8(AF_INET),
              String(cString: current.pointee.ifa_name) == "en0" else { continue }
        var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
        let result = getnameinfo(
            address,
            socklen_t(address.pointee.sa_len),
            &host,
            socklen_t(host.count),
            nil,
            0,
            NI_NUMERICHOST
        )
        if result == 0 {
            let prefixLength: Int
            if let netmask = current.pointee.ifa_netmask,
               netmask.pointee.sa_family == UInt8(AF_INET) {
                let value = netmask.withMemoryRebound(to: sockaddr_in.self, capacity: 1) {
                    UInt32(bigEndian: $0.pointee.sin_addr.s_addr)
                }
                prefixLength = ipv4PrefixLength(netmask: value) ?? defaultLanScanPrefixLength
            } else {
                prefixLength = defaultLanScanPrefixLength
            }
            return LocalWifiIPv4Network(
                address: String(cString: host),
                prefixLength: prefixLength
            )
        }
    }
    return nil
}

/// Cross-platform, privacy-preserving fingerprint for the local IPv4 /24.
/// Only a truncated hash is persisted or sent; the raw network address is not.
func lanNetworkId(ipv4Address: String?) -> String? {
    guard let ipv4Address else { return nil }
    return coreLanNetworkIdForIpv4(address: ipv4Address)
}

func subnet24Hosts(localAddress: String) -> [String] {
    lanSubnetHosts(localAddress: localAddress, prefixLength: defaultLanScanPrefixLength)
}
