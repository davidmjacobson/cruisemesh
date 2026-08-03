import SwiftUI

/// A waiting shared-card request, plus what the sheet needs to describe it.
struct PendingSharedRequestState: Identifiable {
    let request: PendingSharedRequest
    /// The sharer's contact name, or their formatted UserID if we somehow no
    /// longer have them.
    let sharerLabel: String
    /// How many times this person has already been sent away with **Not now**.
    /// From the second dismissal on, the sheet offers a way out for good.
    let dismissalCount: UInt32
    var id: String { UserIdHex.encode(request.requesterUserId) }
}

/// Somebody a friend passed our card to is asking to connect
/// (specs/share-contact.md). Shaped like the friend confirmation, with two
/// deliberate differences: nothing has been imported yet, and there is no
/// **Block** — blocking stays in the contact's details, so a child tapping
/// through a prompt cannot silently sever a relationship.
struct PendingSharedRequestView: View {
    let state: PendingSharedRequestState
    let onConnect: () -> Void
    let onNotNow: () -> Void
    let onNeverAsk: () -> Void

    private var request: PendingSharedRequest { state.request }

    var body: some View {
        ScrollView {
            VStack(spacing: 18) {
                Text("\(request.name) wants to connect")
                    .font(.title.bold())
                    .multilineTextAlignment(.center)
                // Honest provenance, never a verified badge: this says who
                // passed the card along, not who anybody is in real life.
                Text("Shared by \(state.sharerLabel)")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                VStack(spacing: 10) {
                    AvatarView(userId: request.requesterUserId, name: request.name, size: 72)
                    Text(request.name).font(.title2.bold())
                    Text(formatUserId(userId: request.requesterUserId))
                        .font(.body.monospaced())
                        .foregroundStyle(.secondary)
                }
                SafetyWordsRow(label: request.name, userId: request.requesterUserId)
                Button("Connect", action: onConnect)
                    .buttonStyle(.borderedProminent)
                Button("Not now", action: onNotNow)
                // A prompt whose primary action is Connect and that can be
                // re-raised forever is a war of attrition. From the second ask
                // there is a quiet way out: no notification, nothing that reads
                // as a rebuke, cleared by scanning that person's own code.
                if state.dismissalCount >= 1 {
                    Button("Don't ask again", role: .destructive, action: onNeverAsk)
                }
            }
            .padding(24)
        }
        .presentationDetents([.medium, .large])
    }
}
