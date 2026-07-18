import Darwin
import Foundation

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

/// The active Wi-Fi IPv4 address. iOS normally exposes Wi-Fi as `en0`.
func localWifiIPv4Address() -> String? {
    localWifiIPv4Network()?.address
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
