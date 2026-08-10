import CryptoKit
import Foundation
import os.log

struct RemotePushRegistrationPayload: Codable, Equatable {
    let deviceToken: String
    let hints: [String]

    enum CodingKeys: String, CodingKey {
        case deviceToken = "device_token"
        case hints
    }
}

enum RemoteNotificationTokenStore {
    private static let tokenKey = "cruisemesh.apns.device-token"
    private static let registrationFingerprintKey = "cruisemesh.apns.registration-fingerprint"

    static func save(_ token: Data) {
        AppDefaults.current.set(token.map { String(format: "%02x", $0) }.joined(), forKey: tokenKey)
        AppDefaults.current.removeObject(forKey: registrationFingerprintKey)
    }

    static func load() -> String? {
        AppDefaults.current.string(forKey: tokenKey)
    }

    static func registeredFingerprint() -> String? {
        AppDefaults.current.string(forKey: registrationFingerprintKey)
    }

    static func markRegistered(fingerprint: String) {
        AppDefaults.current.set(fingerprint, forKey: registrationFingerprintKey)
    }
}

/// Registers this installation's APNs token against the same salted recipient
/// hints the relay WebSocket watches. The relay receives no contact, group, or
/// message plaintext: a matching sealed-envelope deposit produces only a
/// content-available doorbell, and the normal authenticated fetch/decrypt/ack
/// pass remains the sole delivery path.
enum RemotePushRegistrationClient {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "RemotePushRegistration")
    private static let stateQueue = DispatchQueue(label: "com.cruisemesh.push-registration")
    private static var inFlightFingerprints: Set<String> = []

    static var urlSession: URLSession = .shared

    static func syncCurrentIfPossible() {
        guard TermsAcceptanceStore.isCurrentVersionAccepted(),
              OnboardingStore.isCompleted(),
              let config = RelayConfigStore.load()
        else { return }
        let identity = IdentityStore.loadOrCreate()
        sync(config: config, ownUserId: identity.userId)
    }

    static func sync(config: RelayConfig, ownUserId: Data) {
        guard let token = RemoteNotificationTokenStore.load() else { return }
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let store = AppStore.get()
        let hints = (try? store.relaySelfPushHints(ownUserId: ownUserId, nowMs: now))
            ?? recentHintsFor(userId: ownUserId, nowMs: now)
        guard let request = buildRequest(config: config, deviceToken: token, hints: hints) else { return }
        let fingerprint = registrationFingerprint(config: config, token: token, hints: hints)
        guard RemoteNotificationTokenStore.registeredFingerprint() != fingerprint else { return }

        stateQueue.async {
            guard inFlightFingerprints.insert(fingerprint).inserted else { return }
            let task = urlSession.dataTask(with: request) { _, response, error in
                stateQueue.async {
                    inFlightFingerprints.remove(fingerprint)
                    if let error {
                        log.warning("APNs relay registration failed: \(error.localizedDescription, privacy: .public)")
                        return
                    }
                    guard let http = response as? HTTPURLResponse,
                          (200..<300).contains(http.statusCode) else {
                        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                        log.warning("APNs relay registration returned HTTP \(status, privacy: .public)")
                        return
                    }
                    RemoteNotificationTokenStore.markRegistered(fingerprint: fingerprint)
                    log.info("APNs relay wake registration refreshed")
                }
            }
            task.resume()
        }
    }

    static func buildRequest(config: RelayConfig, deviceToken: String, hints: [Data]) -> URLRequest? {
        let normalized = normalizeRelayUrl(config.relayUrl)
        guard !normalized.isEmpty,
              let url = URL(string: normalized + "/push/registrations")
        else { return nil }
        let payload = RemotePushRegistrationPayload(
            deviceToken: deviceToken,
            hints: Array(Set(hints.map(base64URLEncode))).sorted()
        )
        guard let body = try? JSONEncoder().encode(payload) else { return nil }

        var request = URLRequest(url: url, timeoutInterval: 10)
        request.httpMethod = "PUT"
        request.setValue("Bearer \(config.relayToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        RelayClient.stampTransportHeaders(on: &request)
        request.httpBody = body
        return request
    }

    private static func registrationFingerprint(config: RelayConfig, token: String, hints: [Data]) -> String {
        let material = ([normalizeRelayUrl(config.relayUrl), config.relayToken, token]
            + hints.map(base64URLEncode).sorted()).joined(separator: "|")
        return SHA256.hash(data: Data(material.utf8))
            .map { String(format: "%02x", $0) }
            .joined()
    }

    private static func base64URLEncode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
