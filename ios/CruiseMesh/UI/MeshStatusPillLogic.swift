import Foundation

/// Which internet-delivery service a saved setup card points at, so the pill
/// names it the way the person set it up. Mirrors Android's
/// `InternetDeliveryService`.
enum InternetDeliveryService: Equatable {
    case shorePass
    case customRelay

    var displayName: String {
        switch self {
        case .shorePass: return String(localized: "Shore Pass")
        case .customRelay: return String(localized: "Internet delivery")
        }
    }

    /// The service a saved card points at, or nil when nothing is saved.
    /// `relaySetupIsOfficial` is the core's own check, so both shells agree on
    /// what counts as the official service.
    static func of(_ config: RelayConfig?) -> InternetDeliveryService? {
        guard let config else { return nil }
        return relaySetupIsOfficial(relayUrl: config.relayUrl) ? .shorePass : .customRelay
    }
}

/// Which semantic dot color the mesh status pill shows. Mirrors Android's
/// `MeshStatusDotColor`, value for value, because the two pills now take their
/// severity from the same core verdict and must not draw it differently.
enum MeshStatusDotColor: Equatable {
    case green
    case blue
    case amber
    case neutral
}

/// Everything the pill renders, decided once.
struct MeshStatusPillStatus: Equatable {
    let text: String
    let dot: MeshStatusDotColor
    /**
     The core's verdict on this device's connection
     (`coreClassifyConnectionHealth`).

     Carried on the record rather than kept private so the pill's state is
     inspectable in tests: the property that matters is that it is the *same*
     value the Connection details health card renders, and a value nobody can
     read is a property nobody can pin.
     */
    let health: CoreConnectionHealth
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
    /// Reduce Motion wins over a status that would otherwise pulse. The words,
    /// color, and tap action remain unchanged, so no state is lost with motion.
    static func shouldAnimate(statusWantsPulse: Bool, reduceMotion: Bool) -> Bool {
        statusWantsPulse && !reduceMotion
    }

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
        // The grace window is not given its own suffix: the pill has room for
        // three words, and the asymmetry it would have to explain is spelled
        // out on the pass screens instead.
        case .expired, .expiredReadOnly:
            return String(localized: "\(name) expired")
        case .suspended:
            return String(localized: "\(name) suspended")
        case .tokenRejected:
            return String(localized: "\(name) setup was not accepted")
        case .quotaFull:
            return String(localized: "\(name) storage is full")
        case .messageTooLarge:
            return String(localized: "A message was too large to send")
        // A roaming deferral is a deliberate wait, so it belongs with the
        // states that ask nothing of the user, never with the ones above.
        case .ok, .checking, .noInternet, .deferredRoaming, .noConfig, .failing, .rateLimited:
            return nil
        }
    }

    /**
     The whole pill: its words, its dot, and the verdict behind the dot.

     **The severity is not decided here.** It comes from
     `coreClassifyConnectionHealth` -- the same call, on the same inputs, that
     produces the Connection details health card -- so the pill and that page
     cannot claim different things about the same phone. Before this, the iOS
     pill had no relay-health layer at all beyond the action-required suffix:
     its dot was the mesh runtime and nothing else, so a phone with a friend in
     the room and a pass that had run out was green over a page reading
     `Working, with limits` about that same phone at that same moment.

     The *words* remain this file's own, and remain far quieter than Android's,
     for the reason documented above: a pill is a one-line summary and the card
     is a paragraph. What must agree is the verdict, not the wording.

     - Parameter runtimeText: `MeshRuntimeStatus.pillText`, passed in so this
       stays a pure function of its arguments.
     - Parameter nearbyCount: peers with a live direct link.
     - Parameter checkingSinceMs: when the current unresolved check began, from
       a `CheckingClock` held across renders. Zero means nothing is pending.
     */
    static func build(
        runtimeState: MeshRuntimeState,
        runtimeText: String,
        nearbyCount: Int,
        bluetooth: BluetoothAvailability,
        lanListening: Bool,
        relayHealth: RelayHealth,
        service: InternetDeliveryService?,
        checkingSinceMs: Int64,
        nowMs: Int64
    ) -> MeshStatusPillStatus {
        let relay = ConnectionInputs.relay(relayHealth, configured: service != nil)
        let report = coreClassifyConnectionHealth(
            input: CoreConnectionHealthInput(
                runtime: ConnectionInputs.runtime(runtimeState, bluetooth: bluetooth),
                bluetooth: ConnectionInputs.bluetooth(runtimeState, availability: bluetooth),
                // The pill counts *peers*, not per-radio links, and the counts
                // feed only the core's evidence record, which the pill does not
                // render. Splitting a number the pill does not have would be
                // inventing evidence to look thorough.
                bluetoothLinks: 0,
                localWifi: ConnectionInputs.localWifi(runtimeState, listening: lanListening),
                localWifiLinks: 0,
                relay: relay,
                validatedInternet: ConnectionInputs.validatedInternet(relayHealth),
                nearbyFriendCount: UInt32(clamping: nearbyCount),
                checkingSinceMs: checkingSinceMs,
                nowMs: nowMs
            )
        )
        let suffix = faultSuffix(
            runtimeState: runtimeState,
            relayHealth: relayHealth,
            service: service
        )
        let text = suffix.map { String(localized: "\(runtimeText) · \($0)") } ?? runtimeText
        return MeshStatusPillStatus(
            text: text,
            dot: dot(report.state, nearbyCount: nearbyCount, relay: report.evidence.relay),
            health: report.state
        )
    }

    /**
     The dot, from the core's verdict.

     `ready` still distinguishes green from blue from neutral, because those
     three are not severities: they say which path is carrying, and a person who
     is used to green meaning "someone is here" would lose that reading if every
     healthy state looked alike. Everything that *is* a severity comes from the
     core, so a degraded phone can no longer show green just because a friend
     happens to be in the room.
     */
    private static func dot(
        _ state: CoreConnectionHealth,
        nearbyCount: Int,
        relay: CoreRelayPathState
    ) -> MeshStatusDotColor {
        switch state {
        // No verdict yet is not a warning. The card shows a spinner here; the
        // pill has no room for one, and a colored dot would be a claim.
        case .checking:
            return .neutral
        case .limited, .needsAttention:
            return .amber
        case .ready:
            if nearbyCount > 0 { return .green }
            if relay == CoreRelayPathState.connected { return .blue }
            // Listening, with nobody here and nothing to report. This is the
            // ordinary state of a phone on a quiet morning and must not look
            // like a problem.
            return .neutral
        }
    }
}
