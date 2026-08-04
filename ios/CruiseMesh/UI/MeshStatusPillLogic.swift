import Foundation

/// Which internet-delivery service a saved setup card points at, so the pill
/// names it the way the person set it up. Mirrors Android's
/// `InternetDeliveryService`.
enum InternetDeliveryService: Equatable {
    case cruisePass
    case customRelay

    var displayName: String {
        switch self {
        case .cruisePass: return String(localized: "Shore Pass")
        case .customRelay: return String(localized: "Internet delivery")
        }
    }

    /// The service a saved card points at, or nil when nothing is saved.
    /// `relaySetupIsOfficial` is the core's own check, so both shells agree on
    /// what counts as the official service.
    static func of(_ config: RelayConfig?) -> InternetDeliveryService? {
        guard let config else { return nil }
        return relaySetupIsOfficial(relayUrl: config.relayUrl) ? .cruisePass : .customRelay
    }
}

/// Whether the mesh status pill should say that internet delivery has stopped,
/// and in what words.
///
/// Kept out of `MeshStatusPill` so it is testable without a SwiftUI host --
/// same pattern as `ChatListLogic`.
///
/// ## Why this says so much less than Android's pill
///
/// Android appends a relay-health suffix for *every* health: "internet
/// delivery ✓", "checking Shore Pass", "no internet", and so on. This speaks
/// only when internet delivery has stopped in a way the person has to do
/// something about, and otherwise leaves the pill exactly as it was.
///
/// That is the product bar ("obvious for family members on the surface"),
/// not an omission:
///
/// - **Never set up** is the free default. Nearby delivery is the whole
///   arrangement and it is working. Reporting it as a fault teaches people to
///   ignore the dot for when it finally means something -- the exact defect
///   Android had to remove.
/// - **No internet** is the normal case at sea. This app exists for it.
/// - **Checking** would flicker on every service start and says nothing.
/// - **Working** needs no announcement; the Settings row already shows a green
///   check to anyone who goes looking.
///
/// `PassIndicator` already draws exactly this line for the Settings row --
/// `.actionRequired` means "stays broken until a person acts", as distinct
/// from `.attention`, which clears on its own -- so the gate is that same
/// classification rather than a second opinion about it. Keep them in step: a
/// health that becomes action-required there needs wording here, or the pill
/// silently swallows it.
enum MeshStatusPillLogic {
    /// The text to append to the pill, or nil to leave the pill untouched.
    ///
    /// Only while the mesh is meshing: a stopped mesh is a more immediate
    /// problem than a lapsed pass, and stacking the two reads as noise.
    static func faultSuffix(
        runtimeState: MeshRuntimeState,
        relayHealth: RelayHealth,
        service: InternetDeliveryService?
    ) -> String? {
        guard case .meshing = runtimeState, let service else { return nil }
        guard PassIndicator.of(relayHealth, configured: true) == .actionRequired else { return nil }
        let name = service.displayName
        switch relayHealth {
        case .expired:
            return String(localized: "\(name) expired")
        case .suspended:
            return String(localized: "\(name) suspended")
        case .tokenRejected:
            return String(localized: "\(name) setup was not accepted")
        case .quotaFull:
            return String(localized: "\(name) storage is full")
        case .messageTooLarge:
            return String(localized: "A message was too large to send")
        case .ok, .checking, .noInternet, .noConfig, .failing, .rateLimited:
            return nil
        }
    }
}
