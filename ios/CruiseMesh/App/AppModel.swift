import Combine
import Foundation
import SwiftUI

@MainActor
final class AppModel: ObservableObject {
    // FI3: not `private` so `AppDelegate` (CruiseMeshApp.swift) can check
    // the same persisted preference before `AppModel` itself exists, on a
    // background BLE-restoration relaunch.
    static let meshEnabledKey = "cruisemesh.mesh.enabled"

    let identity: Identity
    @Published var displayName: String
    @Published private(set) var meshEnabled: Bool
    @Published var pendingFriendToken: String?
    @Published var pendingRelayCard: String?
    /// A tapped LAN address link waiting for the person's answer. See
    /// `LanEndpointLinkDecision` for why a link never dials on its own.
    @Published private(set) var pendingLanLink: LanManualEndpoint?
    /// What `pendingLanLink` may do right now, re-derived whenever the mesh
    /// or this phone's Wi-Fi address changes. A link opened while the mesh is
    /// still coming up therefore turns into a question once the listener has
    /// an address, instead of staying a "not on Wi-Fi" refusal.
    @Published private(set) var lanLinkDecision: LanEndpointLinkDecision?

    init() {
        try? BackupService.installPendingRestoreIfNeeded()
        let id = IdentityStore.loadOrCreate()
        UITestConfiguration.prepareFixtures(identity: id)
        self.identity = id
        self.displayName = ProfileStore.loadDisplayName()
        self.pendingFriendToken = nil
        self.pendingRelayCard = nil
        self.pendingLanLink = nil
        self.lanLinkDecision = nil
        if UITestConfiguration.isEnabled {
            self.meshEnabled = false
        } else if AppDefaults.current.object(forKey: Self.meshEnabledKey) == nil {
            self.meshEnabled = true
        } else {
            self.meshEnabled = AppDefaults.current.bool(forKey: Self.meshEnabledKey)
        }
        if !UITestConfiguration.isEnabled {
            MeshController.shared.configure(identity: id)
        }
        // `LanTransportDiagnostics` publishes on the main queue (see its
        // `publish`), so this lands on the main actor without a hop.
        Publishers.CombineLatest3($pendingLanLink, $meshEnabled, LanTransportDiagnostics.shared.$snapshot)
            .map { endpoint, meshEnabled, snapshot in
                endpoint.map {
                    Self.decideLanLink($0, meshEnabled: meshEnabled, snapshot: snapshot)
                }
            }
            .removeDuplicates()
            .assign(to: &$lanLinkDecision)
    }

    func startMesh() {
        guard TermsAcceptanceStore.isCurrentVersionAccepted() else { return }
        meshEnabled = true
        AppDefaults.current.set(true, forKey: Self.meshEnabledKey)
        guard !UITestConfiguration.isEnabled else { return }
        MessageNotifier.requestPermission()
        MeshController.shared.start()
    }

    func startMeshIfEnabled() {
        guard !UITestConfiguration.isEnabled,
              meshEnabled,
              TermsAcceptanceStore.isCurrentVersionAccepted() else { return }
        MeshController.shared.start()
    }

    func stopMesh() {
        meshEnabled = false
        AppDefaults.current.set(false, forKey: Self.meshEnabledKey)
        guard !UITestConfiguration.isEnabled else { return }
        MeshController.shared.stop()
    }

    /// A LAN address link arrived: hold it and show what it may do.
    /// Nothing is dialed and the mesh is never started from here.
    func handleLanEndpointLink(_ endpoint: LanManualEndpoint) {
        pendingLanLink = endpoint
    }

    /// The person said yes to the address. Ask again at that moment: the mesh
    /// or the Wi-Fi network may have changed since the alert last drew. A
    /// changed answer keeps the link, so the alert shows the reason instead.
    func confirmLanEndpointLink() {
        guard let endpoint = pendingLanLink,
              case let .confirm(confirmed) = Self.decideLanLink(
                  endpoint,
                  meshEnabled: meshEnabled,
                  snapshot: LanTransportDiagnostics.shared.snapshot
              )
        else { return }
        pendingLanLink = nil
        LanTransportDiagnostics.shared.queueManualConnection(confirmed)
    }

    /// The confirm alert (`fromConfirmAlert`) or the refusal alert went away
    /// (Cancel, Close, Connect, or a tap outside). Drop the link only if that
    /// alert still matches the current answer; when the answer has changed
    /// underneath it, the other alert takes over and must keep the link.
    func dismissLanEndpointLink(fromConfirmAlert: Bool) {
        guard let current = lanLinkDecision, current.isConfirm == fromConfirmAlert else { return }
        pendingLanLink = nil
    }

    nonisolated private static func decideLanLink(
        _ endpoint: LanManualEndpoint,
        meshEnabled: Bool,
        snapshot: LanTransportSnapshot
    ) -> LanEndpointLinkDecision {
        decideLanEndpointLink(
            endpoint,
            meshRunning: meshEnabled,
            localEndpoint: snapshot.localEndpoint,
            localPrefixLength: snapshot.localPrefixLength
        )
    }

    func setAppForeground(_ foreground: Bool) {
        guard !UITestConfiguration.isEnabled else { return }
        MeshController.shared.setAppForeground(foreground)
    }
}
