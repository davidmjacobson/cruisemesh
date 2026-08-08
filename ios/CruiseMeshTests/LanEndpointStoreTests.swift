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

    /// This phone's own address throughout: 192.168.86.0/24, the network the
    /// field report's dial loop ran on.
    private let localHost = "192.168.86.31"

    func testEndpointCacheIsScopedAndExpiresThroughCorePolicy() {
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        let endpoint = LanManualEndpoint(host: "192.168.86.23", port: 45_892)
        LanEndpointCache.save(
            networkId: networkId,
            userId: userId,
            endpoint: endpoint,
            provenance: .hinted,
            nowMs: 1_000
        )

        XCTAssertEqual(
            LanEndpointCache.load(
                networkId: networkId,
                userId: userId,
                localHost: localHost,
                nowMs: 2_000
            ),
            endpoint
        )
        XCTAssertNil(
            LanEndpointCache.load(
                networkId: networkId,
                userId: userId,
                localHost: localHost,
                nowMs: 1_000 + 7 * 24 * 60 * 60_000 + 1
            )
        )
    }

    func testAnEndpointCachedByAnOlderBuildIsDroppedUnlessItIsALocalAddress() {
        let savedAt: Int64 = 1_000
        let now: Int64 = 2_000
        // An entry written in the pre-provenance format, with a name for a
        // host, is never handed back to be dialed.
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        writeLegacyValue(
            networkId: networkId,
            userId: userId,
            endpoint: LanManualEndpoint(host: "phone.local", port: 45_892),
            savedAtMs: savedAt
        )
        XCTAssertNil(LanEndpointCache.load(
            networkId: networkId,
            userId: userId,
            localHost: localHost,
            nowMs: now
        ))
        XCTAssertNil(storedValue(networkId: networkId, userId: userId))
    }

    func testAValueFromABuildWithoutProvenanceReadsAsUnproven() {
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        writeLegacyValue(
            networkId: networkId,
            userId: userId,
            endpoint: LanManualEndpoint(host: "192.168.86.23", port: 45_892),
            savedAtMs: 1_000
        )

        // Reading it migrates the JSON blob into the shared format, and the
        // conservative reading of a value that never recorded provenance is
        // "unproven": those builds filed hints and proven addresses alike.
        XCTAssertEqual(
            LanEndpointCache.load(
                networkId: networkId,
                userId: userId,
                localHost: localHost,
                nowMs: 2_000
            ),
            LanManualEndpoint(host: "192.168.86.23", port: 45_892)
        )
        let migrated = lanEndpointCacheDecode(value: storedValue(networkId: networkId, userId: userId) ?? "")
        XCTAssertEqual(migrated?.provenance, .hinted)
        XCTAssertEqual(migrated?.host, "192.168.86.23")
        XCTAssertEqual(migrated?.savedAtMs, 1_000)
    }

    func testAPoisonedEntryFromAShippedBuildIsEvictedOnLoad() {
        // The field case: a hint naming 10.80.209.68 was filed under the id of
        // the 192.168.86.0/24 network this phone is on, and cost a connect
        // timeout on every Wi-Fi join for the seven days it lived.
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        writeLegacyValue(
            networkId: networkId,
            userId: userId,
            endpoint: LanManualEndpoint(host: "10.80.209.68", port: 45_892),
            savedAtMs: 1_000
        )

        XCTAssertNil(LanEndpointCache.load(
            networkId: networkId,
            userId: userId,
            localHost: localHost,
            nowMs: 2_000
        ))
        XCTAssertNil(
            storedValue(networkId: networkId, userId: userId),
            "the entry is deleted, not left to age out"
        )
    }

    func testAnAuthenticatedEntryOnAnotherSubnetSurvives() {
        // A peer reached over a routed LAN is legitimately cross-subnet: the
        // handshake is proof the address answers from here, which no amount of
        // subnet comparison can supply.
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        let endpoint = LanManualEndpoint(host: "10.80.209.68", port: 45_892)
        LanEndpointCache.save(
            networkId: networkId,
            userId: userId,
            endpoint: endpoint,
            provenance: .authenticated,
            nowMs: 1_000
        )

        XCTAssertEqual(
            LanEndpointCache.load(
                networkId: networkId,
                userId: userId,
                localHost: localHost,
                nowMs: 2_000
            ),
            endpoint
        )
    }

    func testAHandshakePromotesAStoredHintInPlace() {
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        let endpoint = LanManualEndpoint(host: "10.80.209.68", port: 45_892)
        writeLegacyValue(networkId: networkId, userId: userId, endpoint: endpoint, savedAtMs: 1_000)

        LanEndpointCache.save(
            networkId: networkId,
            userId: userId,
            endpoint: endpoint,
            provenance: .authenticated,
            nowMs: 1_000
        )
        XCTAssertEqual(
            lanEndpointCacheDecode(value: storedValue(networkId: networkId, userId: userId) ?? "")?.provenance,
            .authenticated
        )

        // A hint repeating the same proven address must not undo the proof.
        LanEndpointCache.save(
            networkId: networkId,
            userId: userId,
            endpoint: endpoint,
            provenance: .hinted,
            nowMs: 1_500
        )
        XCTAssertEqual(
            LanEndpointCache.load(
                networkId: networkId,
                userId: userId,
                localHost: localHost,
                nowMs: 2_000
            ),
            endpoint
        )
    }

    func testAnUnprovenEntryIsKeptWhenThisPhoneHasNothingToCompareWith() {
        // Not dialing is enough to stop the loop; deleting on a load that
        // cannot judge the entry would throw away a usable address because the
        // local interface happened to be unreadable.
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        writeLegacyValue(
            networkId: networkId,
            userId: userId,
            endpoint: LanManualEndpoint(host: "10.80.209.68", port: 45_892),
            savedAtMs: 1_000
        )

        XCTAssertNil(LanEndpointCache.load(
            networkId: networkId,
            userId: userId,
            localHost: nil,
            nowMs: 2_000
        ))
        XCTAssertNotNil(storedValue(networkId: networkId, userId: userId))
    }

    func testAnUnreadableStoredValueIsDiscarded() {
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        let key = storageKey(networkId: networkId, userId: userId)

        // A string in no format this build knows.
        UserDefaults.standard.set("not-a-cache-entry", forKey: key)
        XCTAssertNil(LanEndpointCache.load(
            networkId: networkId,
            userId: userId,
            localHost: localHost,
            nowMs: 2_000
        ))
        XCTAssertNil(UserDefaults.standard.object(forKey: key))

        // And a blob that is not the JSON older builds wrote. This one used to
        // survive: it read as neither shape, so the load returned before
        // reaching the delete and re-rejected the same bytes on every Wi-Fi
        // join for as long as the app stayed installed.
        UserDefaults.standard.set(Data([0x00, 0x01, 0x02]), forKey: key)
        XCTAssertNil(LanEndpointCache.load(
            networkId: networkId,
            userId: userId,
            localHost: localHost,
            nowMs: 2_000
        ))
        XCTAssertNil(UserDefaults.standard.object(forKey: key))
    }

    func testAValueCarryingAFieldThisBuildDoesNotKnowIsStillRead() {
        // Room for the next append: a value with a fifth field must not read
        // as unreadable, or adding one later would wipe the cache on any phone
        // that rolls back to this build.
        let networkId = "test-\(UUID().uuidString)"
        let userId = uuidData()
        let endpoint = LanManualEndpoint(host: "10.80.209.68", port: 45_892)
        let current = lanEndpointCacheEncode(entry: LanEndpointCacheEntry(
            host: endpoint.host,
            port: endpoint.port,
            savedAtMs: 1_000,
            provenance: .authenticated
        ))
        UserDefaults.standard.set(
            current + "|whatever-comes-next",
            forKey: storageKey(networkId: networkId, userId: userId)
        )

        XCTAssertEqual(
            LanEndpointCache.load(
                networkId: networkId,
                userId: userId,
                localHost: localHost,
                nowMs: 2_000
            ),
            endpoint
        )
    }

    private struct LegacyCachedEndpoint: Codable {
        let endpoint: LanManualEndpoint
        let savedAtMs: Int64
    }

    /// Writes the exact bytes a build without provenance wrote.
    private func writeLegacyValue(
        networkId: String,
        userId: Data,
        endpoint: LanManualEndpoint,
        savedAtMs: Int64
    ) {
        let legacy = try? JSONEncoder().encode(
            LegacyCachedEndpoint(endpoint: endpoint, savedAtMs: savedAtMs)
        )
        UserDefaults.standard.set(legacy, forKey: storageKey(networkId: networkId, userId: userId))
    }

    private func storedValue(networkId: String, userId: Data) -> String? {
        UserDefaults.standard.string(forKey: storageKey(networkId: networkId, userId: userId))
    }

    private func storageKey(networkId: String, userId: Data) -> String {
        "cruisemesh.lan.endpoint.\(networkId).\(UserIdHex.encode(userId))"
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
