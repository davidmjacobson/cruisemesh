import Combine
import XCTest
@testable import CruiseMesh

/// Guards the two threading promises the mesh pipeline's move off the main
/// thread rests on.
///
/// `MeshController` now runs every inbound frame, connect, disconnect and
/// timer tick on its own serial queue, so anything it signals outward has to
/// be safe from there. Neither of these is observable in the app until it is
/// wrong, at which point it is a UI update on a background thread or a
/// contact recorded as "met in person" who was never nearby.
final class MeshEventThreadingTests: XCTestCase {
    private var cancellables: Set<AnyCancellable> = []

    override func tearDown() {
        cancellables.removeAll()
        MeshRouter.reset()
        super.tearDown()
    }

    private func userId(_ byte: UInt8) -> Data { Data(repeating: byte, count: 16) }

    // MARK: - Chat events reach subscribers on the main thread

    /// `ChatListView` and `ChatView` `sink` on these subjects without a
    /// `receive(on:)` of their own and reload SwiftUI state in the closure, so
    /// the send has to arrive on the main thread whatever thread published it.
    func testChatChangedFromABackgroundThreadIsDeliveredOnMain() {
        let delivered = expectation(description: "chat change delivered")
        var deliveredOnMain: Bool?
        ChatEvents.subject
            .sink { _ in
                deliveredOnMain = Thread.isMainThread
                delivered.fulfill()
            }
            .store(in: &cancellables)

        DispatchQueue(label: "test.mesh.pipeline").async {
            ChatEvents.notifyChatChanged(self.userId(1))
        }

        wait(for: [delivered], timeout: 2)
        XCTAssertEqual(deliveredOnMain, true)
    }

    func testRelaySyncRequestFromABackgroundThreadIsDeliveredOnMain() {
        let delivered = expectation(description: "sync request delivered")
        var deliveredOnMain: Bool?
        RelaySyncEvents.subject
            .sink {
                deliveredOnMain = Thread.isMainThread
                delivered.fulfill()
            }
            .store(in: &cancellables)

        DispatchQueue(label: "test.mesh.pipeline").async {
            RelaySyncEvents.requestSync()
        }

        wait(for: [delivered], timeout: 2)
        XCTAssertEqual(deliveredOnMain, true)
    }

    /// A send that is already on the main thread stays synchronous, so a
    /// UI-driven change still reloads within the same run loop turn.
    func testChatChangedFromMainIsDeliveredSynchronously() {
        var delivered = false
        ChatEvents.subject
            .sink { _ in delivered = true }
            .store(in: &cancellables)

        ChatEvents.notifyChatChanged(userId(2))

        XCTAssertTrue(delivered)
    }

    // MARK: - "Nearby" agrees between the router and the published copy

    /// The pipeline reads "was this peer in range when we accepted them" from
    /// `MeshRouter` rather than from `MeshConnectivityStatus.nearbyPeerIds`,
    /// which is main-actor state it can no longer read synchronously. That is
    /// only sound while the two agree -- the published set is derived from the
    /// router's identified routes and nothing else.
    @MainActor
    func testNearbyPeerIdsAreExactlyTheRoutersIdentifiedRoutes() {
        MeshRouter.reset()
        let alice = userId(1)
        let bob = userId(2)

        MeshRouter.onConnected(address: "AA:BB", transport: .central)
        MeshRouter.onHello(address: "AA:BB", userId: alice)
        // Connected but never identified: in neither set.
        MeshRouter.onConnected(address: "CC:DD", transport: .peripheral)

        MeshConnectivityStatus.shared.refreshNearbyRoutes()
        XCTAssertEqual(
            MeshConnectivityStatus.shared.nearbyPeerIds,
            Set(MeshRouter.identifiedRoutes().map(\.userId))
        )
        XCTAssertTrue(MeshRouter.identifiedRoutes().contains { $0.userId == alice })
        XCTAssertFalse(MeshRouter.identifiedRoutes().contains { $0.userId == bob })

        MeshRouter.onDisconnected(address: "AA:BB")
        MeshConnectivityStatus.shared.refreshNearbyRoutes()
        XCTAssertEqual(
            MeshConnectivityStatus.shared.nearbyPeerIds,
            Set(MeshRouter.identifiedRoutes().map(\.userId))
        )
        XCTAssertFalse(MeshRouter.identifiedRoutes().contains { $0.userId == alice })

        MeshConnectivityStatus.shared.clear()
    }
}
