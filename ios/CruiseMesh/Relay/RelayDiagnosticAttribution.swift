import Foundation

/// The stable contact identifier used in relay support logs.
///
/// This deliberately mirrors Android's `UserIdHex.encode(contact.userId)`
/// policy: it is enough to map a failure back to the right local contact
/// without putting a mutable display name in a shared diagnostics archive.
func relayDiagnosticContactId(_ userId: Data) -> String {
    UserIdHex.encode(userId)
}

/// A relay address safe to include in shared diagnostics.
///
/// Only the host survives. User info, ports, paths, query parameters (including
/// recipient hints), fragments, and the bearer token all remain out of logs.
func relayDiagnosticHost(_ url: URL?) -> String {
    guard let host = url?.host, !host.isEmpty else { return "unknown" }
    return host.lowercased()
}

func relayDiagnosticHost(_ relayUrl: String?) -> String {
    guard let relayUrl else { return "unknown" }
    return relayDiagnosticHost(URL(string: relayUrl))
}

/// Method/path retain the useful operation name; host identifies the failing
/// relay. No request headers, body, query, or URL credentials are included.
func relayDiagnosticRequestLabel(_ request: URLRequest) -> String {
    let method = request.httpMethod ?? "?"
    let path = request.url?.path ?? "?"
    return "\(method) \(path) host=\(relayDiagnosticHost(request.url))"
}
