import SwiftUI

/// Severity of the Shore Pass status shown in Settings, derived from
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

    /// Amber "?": something is off in a way that clears on its own -- the
    /// service couldn't be reached just now, or asked us to slow down (429).
    /// Worth a glance, never worth acting on, and never a reason to contact
    /// anyone.
    case attention

    /// Red "!": internet delivery stays affected until the person does
    /// something -- renew the pass, replace the setup card, send a smaller
    /// message, or contact support. These states do not self-heal, which is
    /// what separates them from `attention`.
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
        case .noInternet, .deferredRoaming: return .waiting
        // Transient, self-healing ("?"): can't reach right now, or told to
        // slow down. Same reaction either way -- none.
        case .failing, .rateLimited: return .attention
        // Persistent, actionable ("!"): these stay until someone acts.
        case .expired, .expiredReadOnly, .suspended, .tokenRejected, .quotaFull, .messageTooLarge:
            return .actionRequired
        }
    }

    /// SF Symbol for the row. Each state gets a distinct *shape* as well as a
    /// distinct colour so the row still reads correctly for anyone who cannot
    /// tell the tints apart. CP2b (David's UX spec): the "?" circle marks
    /// transient, self-healing conditions; the "!" circle marks persistent
    /// ones that need a person.
    var systemImage: String? {
        switch self {
        case .none: return nil
        case .ready: return "checkmark.circle.fill"
        case .waiting: return "info.circle.fill"
        case .attention: return "questionmark.circle.fill"
        case .actionRequired: return "exclamationmark.circle.fill"
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
        case .ready: return "Shore Pass ready"
        case .waiting: return "Shore Pass waiting for internet"
        case .attention: return "Shore Pass needs attention"
        case .actionRequired: return "Shore Pass needs action"
        }
    }
}

extension RelayHealth {
    /// True when this health is an actual verdict on the pass rather than "we
    /// have not looked yet". `.checking` and `.noConfig` both mean the absence
    /// of an answer -- the latter is what a stopped mesh service leaves
    /// behind, and what a saved-but-unchecked card reports.
    var isPassVerdict: Bool {
        switch self {
        case .checking, .noConfig: return false
        default: return true
        }
    }
}

/// Heading shown at the top of the Shore Pass screen.
///
/// Mirrors Android's `PassIndicator.kt` -- keep the two mappings in step.
enum ShorePassHeading {
    /// No setup card saved: invite them to add one.
    case notSetUp

    /// A card is saved but no check has landed yet.
    case checking

    /// Green check: the relay answered and the pass is good.
    case ready

    /// A card is saved and the last check said something other than OK.
    case configured

    /// Heading for a saved pass, given the live `health` and `lastVerdict` --
    /// the most recent health that was an actual answer (`isPassVerdict`).
    ///
    /// Re-checks are not demotions. A background sync pass, or a service
    /// restart, drops health to `.checking`/`.noConfig` for a second or two;
    /// without `lastVerdict` the heading would fall from "Shore Pass is set
    /// up" with its green check to "Shore Pass is configured" and back, which
    /// reads to the person holding the phone as the pass breaking and healing.
    /// This is the same reasoning that maps those two states to
    /// `PassIndicator.none` rather than to a symbol that would only flicker.
    ///
    /// It stays health-only for every real verdict: the moment the relay
    /// answers with anything but OK -- rejected token, expired, no internet --
    /// the green check goes, because `lastVerdict` is then that answer and not
    /// the stale OK.
    static func of(
        _ health: RelayHealth,
        configured: Bool,
        lastVerdict: RelayHealth?
    ) -> ShorePassHeading {
        guard configured else { return .notSetUp }
        guard let settled = health.isPassVerdict ? health : lastVerdict else { return .checking }
        if case .ok = settled { return .ready }
        return .configured
    }
}
