import XCTest
@testable import CruiseMesh

final class LanEndpointStoreTests: XCTestCase {
    func testEndpointContentRoundTripsThroughSharedCoreEncoding() throws {
        let original = LanEndpointContent(
            instanceToken: Data([1, 2, 3, 4, 5, 6, 7, 8]),
            networkId: Data("network-a".utf8),
            host: "10.0.0.7",
            port: 45_892,
            expiresAtMs: 123_456
        )

        let decoded = try decodeLanEndpointContent(
            bytes: encodeLanEndpointContent(content: original)
        )

        XCTAssertEqual(decoded.instanceToken, original.instanceToken)
        XCTAssertEqual(decoded.networkId, original.networkId)
        XCTAssertEqual(decoded.host, original.host)
        XCTAssertEqual(decoded.port, original.port)
        XCTAssertEqual(decoded.expiresAtMs, original.expiresAtMs)
    }

    func testEndpointCacheIsScopedAndExpiresThroughCorePolicy() {
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        let endpoint = LanManualEndpoint(host: "10.0.0.8", port: 45_892)
        LanEndpointCache.save(networkId: networkId, userId: userId, endpoint: endpoint, nowMs: 1_000)

        XCTAssertEqual(
            LanEndpointCache.load(networkId: networkId, userId: userId, nowMs: 2_000),
            endpoint
        )
        XCTAssertNil(
            LanEndpointCache.load(
                networkId: networkId,
                userId: userId,
                nowMs: 1_000 + 7 * 24 * 60 * 60_000 + 1
            )
        )
    }

    func testAnEndpointCachedByAnOlderBuildIsDroppedUnlessItIsALocalAddress() {
        let savedAt: Int64 = 1_000
        let now: Int64 = 2_000
        // What older builds could have stored: the cache took any string.
        XCTAssertFalse(cachedLanEndpointIsDialable(host: "phone.local", savedAtMs: savedAt, nowMs: now))
        XCTAssertFalse(cachedLanEndpointIsDialable(host: "cruisemesh.app", savedAtMs: savedAt, nowMs: now))
        XCTAssertFalse(cachedLanEndpointIsDialable(host: "8.8.8.8", savedAtMs: savedAt, nowMs: now))
        XCTAssertTrue(cachedLanEndpointIsDialable(host: "10.0.0.7", savedAtMs: savedAt, nowMs: now))

        // And end to end: an entry written in the pre-upgrade format, with a
        // name for a host, is never handed back to be dialed.
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        let legacy = try? JSONEncoder().encode(
            LegacyCachedEndpoint(
                endpoint: LanManualEndpoint(host: "phone.local", port: 45_892),
                savedAtMs: savedAt
            )
        )
        UserDefaults.standard.set(
            legacy,
            forKey: "cruisemesh.lan.endpoint.\(networkId).\(UserIdHex.encode(userId))"
        )
        XCTAssertNil(LanEndpointCache.load(networkId: networkId, userId: userId, nowMs: now))
    }

    private struct LegacyCachedEndpoint: Codable {
        let endpoint: LanManualEndpoint
        let savedAtMs: Int64
    }

    func testEndpointResendDedupeUsesFiveMinuteSignatureWindow() {
        let userId = uuidData()
        let networkId = "test-\(UUID().uuidString)"
        let endpoint = LanManualEndpoint(host: "10.0.0.9", port: 45_892)
        let token = Data([1, 2, 3, 4, 5, 6, 7, 8])

        XCTAssertTrue(LanCapabilityStore.shouldSendEndpoint(
            userId: userId,
            networkId: networkId,
            endpoint: endpoint,
            instanceToken: token,
            nowMs: 1_000
        ))
        XCTAssertFalse(LanCapabilityStore.shouldSendEndpoint(
            userId: userId,
            networkId: networkId,
            endpoint: endpoint,
            instanceToken: token,
            nowMs: 1_000 + 5 * 60_000 - 1
        ))
        XCTAssertTrue(LanCapabilityStore.shouldSendEndpoint(
            userId: userId,
            networkId: networkId,
            endpoint: endpoint,
            instanceToken: token,
            nowMs: 1_000 + 5 * 60_000
        ))
    }

    private func uuidData() -> Data {
        var uuid = UUID().uuid
        return withUnsafeBytes(of: &uuid) { Data($0) }
    }
}
