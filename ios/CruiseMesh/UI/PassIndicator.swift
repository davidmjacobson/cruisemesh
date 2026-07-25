import SwiftUI

/// Severity of the Cruise Pass status shown in Settings, derived from
/// `RelayHealth` plus whether a setup card is saved at all.
///
/// Mirrors Android's `PassIndicator.kt` -- keep the two mappings in step.
/// Deliberately coarser than `RelayHealth`: the row needs one glanceable
/// symbol, and several distinct health states call for the same reaction from
/// the person holding the phone.
enum PassIndicator {
    /// Show nothing. Either no pass is set up -- which is the free default,
    /// where nearby delivery still works and so must never be dressed up as a
    /// fault -- or the first authenticated check has not finished yet, where
    /// a symbol would only flicker.
    case none

    /// Green check: the relay answered and the pass is good.
    case ready

    /// Neutral: the pass is fine, this phone just has no internet right now.
    ///
    /// Never an error. CruiseMesh exists for exactly this situation -- being
    /// at sea with no connectivity is the normal case, not a failure -- and a
    /// red mark here would be both wrong and a fast way to teach people to
    /// ignore the indicator when it finally does mean something.
    case waiting

    /// Amber: something is off in a way that may clear on its own (service
    /// unavailable). Worth a glance, not worth acting on yet.
    case attention

    /// Red: the pass will not work again until the person does something --
    /// renew it, replace the setup card, or contact support. These states do
    /// not self-heal, which is what separates them from `attention`.
    case actionRequired

    /// Map relay health to the Settings indicator. `configured` is whether a
    /// setup card is saved at all, which `RelayHealth` alone cannot express:
    /// a phone that has never had a pass and a phone whose pass is saved but
    /// unchecked can both report `.noConfig`.
    static func of(_ health: RelayHealth, configured: Bool) -> PassIndicator {
        guard configured else { return .none }
        switch health {
        case .noConfig, .checking: return .none
        case .ok: return .ready
        case .noInternet: return .waiting
        case .failing: return .attention
        case .expired, .suspended, .tokenRejected: return .actionRequired
        }
    }

    /// SF Symbol for the row. Each state gets a distinct *shape* as well as a
    /// distinct colour so the row still reads correctly for anyone who cannot
    /// tell the tints apart.
    var systemImage: String? {
        switch self {
        case .none: return nil
        case .ready: return "checkmark.circle.fill"
        case .waiting: return "info.circle.fill"
        case .attention: return "exclamationmark.triangle.fill"
        case .actionRequired: return "xmark.circle.fill"
        }
    }

    var tint: Color {
        switch self {
        case .none, .waiting: return .secondary
        case .ready: return .green
        case .attention: return .orange
        case .actionRequired: return .red
        }
    }

    /// VoiceOver label. The row's title and detail already carry the wording,
    /// so this stays short and is not read as a full sentence.
    var accessibilityLabel: LocalizedStringKey? {
        switch self {
        case .none: return nil
        case .ready: return "Cruise Pass ready"
        case .waiting: return "Cruise Pass waiting for internet"
        case .attention: return "Cruise Pass needs attention"
        case .actionRequired: return "Cruise Pass needs action"
        }
    }
}
