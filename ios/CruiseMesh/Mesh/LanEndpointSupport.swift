import CryptoKit
import Darwin
import Foundation

struct LanManualEndpoint: Codable, Equatable {
    let host: String
    let port: UInt16

    var display: String {
        host.contains(":") ? "[\(host)]:\(port)" : "\(host):\(port)"
    }
}

func parseLanManualEndpoint(_ text: String, defaultPort: UInt16 = lanDefaultTcpPort()) -> LanManualEndpoint? {
    let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !value.isEmpty else { return nil }

    let host: String
    let portText: String?
    if value.hasPrefix("[") {
        guard let closing = value.firstIndex(of: "]"), closing > value.startIndex else { return nil }
        host = String(value[value.index(after: value.startIndex)..<closing])
        let suffix = String(value[value.index(after: closing)...])
        if suffix.isEmpty {
            portText = nil
        } else if suffix.hasPrefix(":"), suffix.count > 1 {
            portText = String(suffix.dropFirst())
        } else {
            return nil
        }
    } else if value.filter({ $0 == ":" }).count == 1 {
        let parts = value.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 2 else { return nil }
        host = String(parts[0])
        portText = String(parts[1])
    } else {
        host = value
        portText = nil
    }

    guard !host.isEmpty, !host.contains(where: \.isWhitespace) else { return nil }
    let port: UInt16
    if let portText {
        guard let parsed = UInt16(portText), parsed > 0 else { return nil }
        port = parsed
    } else {
        port = defaultPort
    }
    return LanManualEndpoint(host: host, port: port)
}

private let lanLinkPrefix = "CMLAN1:"

func lanEndpointLink(_ endpoint: LanManualEndpoint) -> String {
    let encodedHost = Data(endpoint.host.utf8).base64URLEncoded
    return "https://cruisemesh.app/lan#\(lanLinkPrefix)\(encodedHost):\(endpoint.port)"
}

func parseLanEndpointLink(_ fragment: String?) -> LanManualEndpoint? {
    guard let fragment, fragment.hasPrefix(lanLinkPrefix) else { return nil }
    let payload = String(fragment.dropFirst(lanLinkPrefix.count))
    guard let separator = payload.lastIndex(of: ":") else { return nil }
    let encodedHost = String(payload[..<separator])
    let portText = String(payload[payload.index(after: separator)...])
    guard let hostData = Data(base64URL: encodedHost),
          let host = String(data: hostData, encoding: .utf8),
          let port = UInt16(portText),
          port > 0,
          !host.isEmpty,
          !host.contains(where: \.isWhitespace) else { return nil }
    return LanManualEndpoint(host: host, port: port)
}

/// The active Wi-Fi IPv4 address. iOS normally exposes Wi-Fi as `en0`.
func localWifiIPv4Address() -> String? {
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
            return String(cString: host)
        }
    }
    return nil
}

/// Cross-platform, privacy-preserving fingerprint for the local IPv4 /24.
/// Only a truncated hash is persisted or sent; the raw network address is not.
func lanNetworkId(ipv4Address: String?) -> String? {
    guard let ipv4Address else { return nil }
    let octets = ipv4Address.split(separator: ".")
    guard octets.count == 4, octets.allSatisfy({ UInt8($0) != nil }) else { return nil }
    let prefix = "\(octets[0]).\(octets[1]).\(octets[2]).0/24"
    let input = Data("CruiseMesh LAN network v1\u{0}ipv4:\(prefix)".utf8)
    return Data(SHA256.hash(data: input).prefix(16)).base64URLEncoded
}

func subnet24Hosts(localAddress: String) -> [String] {
    let octets = localAddress.split(separator: ".").compactMap { UInt8($0) }
    guard octets.count == 4 else { return [] }
    return (1...254)
        .filter { UInt8($0) != octets[3] }
        .map { "\(octets[0]).\(octets[1]).\(octets[2]).\($0)" }
}

private extension Data {
    var base64URLEncoded: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    init?(base64URL: String) {
        var value = base64URL
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        value.append(String(repeating: "=", count: (4 - value.count % 4) % 4))
        self.init(base64Encoded: value)
    }
}
