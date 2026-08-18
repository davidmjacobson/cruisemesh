import SwiftUI

/// §10.4's changed-safety-state surface, in the pattern the app already has.
///
/// `specs/multi-device-v1.md` §10 note 4 asks for "the standard
/// changed-safety-state surface treatment" when a contact's devices change under
/// them. On this shell that pattern is `IdentityCloneNotice`: a persistent,
/// non-modal banner pinned above the composer, not a dialog and not a row three
/// taps away — because the thing it describes is otherwise silent, and the moment
/// that matters is the moment before somebody types.
///
/// The facts come from `MessageStore.contactSafetyFacts`, which core raises once
/// per stored version. This file adds words and an acknowledgement and decides
/// nothing: which reason applies, which devices it names and when it may stop
/// being shown are all core's, and the fork case is never resolved by arithmetic
/// here any more than it is there.
///
/// # Wording
///
/// No roster, no epoch, no tombstone, no device certificate. A family reads
/// "removed a device", "set up again from a backup", "does not add up", and the
/// one instruction that is actually actionable: check another way before sending
/// anything private.
///
/// The exact twin of Android's `ContactSafetyNotice.kt`.
func contactSafetyCopy(reason: ContactSafetyReason, contactName: String) -> String {
    switch reason {
    case .deviceRevoked:
        return String(
            localized: "\(contactName) removed one of their devices. Messages you send from now on will not reach it."
        )
    case .identityRecovered:
        return String(
            localized: "\(contactName) set up CruiseMesh again from a backup. If they did not do that, check with them another way before you send anything private."
        )
    case .rosterForked:
        return String(
            localized: "Something about \(contactName)'s devices does not add up. Check with them another way before you send anything private."
        )
    }
}

/// Whether this reason is one a person can settle themselves after checking out
/// of band.
///
/// Only the fork is. DL-2 quarantines a contact's roster updates until a person
/// says the fork was resolved, and `MessageStore.clearRosterQuarantine` is the
/// action that says so. The other two reasons are things that happened, not
/// states to be cleared: acknowledging them puts the banner away and changes
/// nothing else.
func offersOutOfBandCheck(reason: ContactSafetyReason) -> Bool {
    reason == .rosterForked
}

/// The fact to show when several are outstanding for one contact.
///
/// The newest by `observedSeq`, which is core's own monotone observation order
/// (deliberately not a wall clock — nothing on the write path has a trustworthy
/// one). Acknowledging it acknowledges everything at or below it, so a person who
/// dismisses the banner is not handed the same contact's older news afterwards.
func latestSafetyFact(facts: [ContactSafetyFact], personUserId: Data) -> ContactSafetyFact? {
    facts
        .filter { $0.personUserId == personUserId && !$0.acknowledged }
        .max { $0.observedSeq < $1.observedSeq }
}

struct ContactSafetyNotice: View {
    let fact: ContactSafetyFact
    let contactName: String
    var onAcknowledge: () -> Void = {}
    var onCheckedOutOfBand: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(contactSafetyCopy(reason: fact.reason, contactName: contactName))
                .font(.caption)
                .frame(maxWidth: .infinity, alignment: .leading)
            HStack(spacing: 16) {
                Button("Got it") { onAcknowledge() }
                    .font(.caption.weight(.semibold))
                if offersOutOfBandCheck(reason: fact.reason) {
                    Button("I checked, it's them") { onCheckedOutOfBand() }
                        .font(.caption.weight(.semibold))
                }
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Color.red.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .padding(.horizontal, 12)
        .padding(.bottom, 4)
        .accessibilityIdentifier("chat.contact-safety")
    }
}
