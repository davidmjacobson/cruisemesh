import SwiftUI

struct FriendPreviewState: Identifiable {
    let contact: Contact
    /// How this card relates to contacts already saved, decided in core so both
    /// shells agree (`friend_card_match`). Supersedes the old free-text
    /// `warning`, which named the wrong person.
    let match: FriendCardMatch
    /// The card came off this phone's camera, which is co-presence by
    /// construction. Recorded in `ContactProvenance.addedNearby`; a pasted or
    /// linked card says nothing about where its owner is, so it defaults false.
    var scanned: Bool = false
    /// Set when this card came out of somebody's **Share contact** code. It
    /// rides back out on the mutual request so the other phone can ask before
    /// importing (specs/share-contact.md).
    var shared: SharedFriendCard? = nil
    /// Who passed the card along, for the "Shared by Mom" line.
    var sharedByLabel: String? = nil
    var id: String { UserIdHex.encode(contact.userId) }
}

struct FriendAddedState: Identifiable {
    let contact: Contact
    let delivery: FriendRequestDelivery
    let relayConfigured: Bool
    /// The card arrived from a shared code, so they have not agreed to
    /// anything yet. Every rejection path is silent by design, so this screen
    /// must not promise a connection it cannot see.
    var awaitingAcceptance: Bool = false
    var id: String { UserIdHex.encode(contact.userId) }
}

struct FriendIdentityBlock: View {
    let contact: Contact

    private var displayName: String { ChatListLogic.contactDisplayName(contact) }

    var body: some View {
        VStack(spacing: 10) {
            AvatarView(
                userId: contact.userId,
                name: displayName,
                size: 72,
                photo: (try? AppStore.get().contactAvatar(userId: contact.userId)).flatMap { UIImage(data: $0) }
            )
            Text(displayName).font(.title2.bold())
            // Safety-word verification moved to the contact's details sheet
            // ("Verify contact") to keep the first-run surface simple (T10).
        }
    }
}

/// One friend's safety words, labelled, so two of them can be read side by side.
struct SafetyWordsRow: View {
    let label: String
    let userId: Data

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(fingerprintWords(userId: userId).joined(separator: " "))
                .font(.body.monospaced())
                .textSelection(.enabled)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct FriendPreviewView: View {
    let state: FriendPreviewState
    let onConfirm: () -> Void
    @Environment(\.dismiss) private var dismiss

    private var isUpdate: Bool {
        if case .alreadySaved = state.match { return true }
        return false
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 18) {
                Text(isUpdate ? "Update this friend?" : "Add this friend?").font(.title.bold())
                // Honest provenance, never a verified badge: whoever shared
                // this card vouched for passing it on, nothing more.
                if let sharedByLabel = state.sharedByLabel {
                    Text("Shared by \(sharedByLabel)")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                FriendIdentityBlock(contact: state.contact)
                matchNote
                Button(isUpdate ? "Update this friend" : "Add this friend", action: onConfirm)
                    .buttonStyle(.borderedProminent)
                Button("Cancel", role: .cancel) { dismiss() }
            }
            .padding(24)
        }
        .presentationDetents([.medium, .large])
    }

    @ViewBuilder
    private var matchNote: some View {
        switch state.match {
        case .new:
            EmptyView()

        case let .alreadySaved(savedName, nameSharedWithOther):
            // Not a warning: the card's UserID is derived from its signing key,
            // so a card already on file is the same person re-sharing.
            VStack(spacing: 8) {
                Text("You already have this friend, saved as \(savedName). Adding the card again just updates how your phone reaches them.")
                    .font(.callout)
                    .multilineTextAlignment(.center)
                if nameSharedWithOther {
                    // Two contacts show the same name, so name alone cannot say
                    // which one this is. The safety words can.
                    VStack(spacing: 6) {
                        Text("Another friend also shows as \(savedName). This card belongs to the one with these safety words:")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                        SafetyWordsRow(label: savedName, userId: state.contact.userId)
                        Text("Give one of them a nickname so you can tell them apart.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                }
            }

        case let .nameTaken(otherUserId, otherName):
            VStack(spacing: 10) {
                Text("You already have a different friend named \(otherName). This card is someone else, with different security keys.")
                    .font(.callout)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                SafetyWordsRow(label: String(localized: "This card"), userId: state.contact.userId)
                SafetyWordsRow(label: otherName, userId: otherUserId)
                Text("Ask them to read their safety words aloud from Profile, Verify my identity. Different words mean different people — add this card and nickname one of them.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
        }
    }
}

struct FriendConfirmationView: View {
    let state: FriendAddedState
    let ownUserId: Data
    let onSayHi: () -> Void
    let onAddAnother: (() -> Void)?
    let onDone: () -> Void
    @State private var connected: Bool
    /// Read once as the sheet is built, and recorded straight away: the hint
    /// is offered at the first friend-added sheet and never again, whether it
    /// is dismissed or the sheet is swiped away with it still on screen.
    @State private var showAirplaneHint: Bool

    init(
        state: FriendAddedState,
        ownUserId: Data,
        onSayHi: @escaping () -> Void,
        onAddAnother: (() -> Void)?,
        onDone: @escaping () -> Void
    ) {
        self.state = state
        self.ownUserId = ownUserId
        self.onSayHi = onSayHi
        self.onAddAnother = onAddAnother
        self.onDone = onDone
        _connected = State(
            initialValue: !state.awaitingAcceptance
                && state.delivery.lamport == 0
                && state.delivery.reachedDirectly
        )
        _showAirplaneHint = State(initialValue: AirplaneDemoHintStore.shouldShow())
    }

    var body: some View {
        VStack(spacing: 18) {
            if state.awaitingAcceptance {
                Text("Request sent").font(.title.bold())
            } else {
                Text(connected ? "You're connected" : "Friend added").font(.title.bold())
            }
            FriendIdentityBlock(contact: state.contact)
            Label(statusText, systemImage: connected ? "checkmark.circle.fill" : "clock.arrow.circlepath")
                .font(.callout)
                .foregroundStyle(connected ? Color.accentColor : .secondary)
            if showAirplaneHint {
                AirplaneDemoHint { showAirplaneHint = false }
                    // Marked only once the hint has actually been on screen:
                    // SwiftUI builds view values eagerly, so doing this in
                    // init would burn the one-time hint for sheets that are
                    // never presented.
                    .onAppear { AirplaneDemoHintStore.markShown() }
            }
            Button("Say hi", action: onSayHi).buttonStyle(.borderedProminent)
            if let onAddAnother { Button("Add another", action: onAddAnother) }
            Button("Done", action: onDone)
        }
        .padding(24)
        .presentationDetents([.medium, .large])
        .interactiveDismissDisabled(false)
        .task(id: state.id) {
            // A delivered receipt would only prove the request arrived, not
            // that they said yes, so a shared import never polls for one.
            while !connected && !state.awaitingAcceptance && state.delivery.lamport > 0 {
                let delivered = (try? AppStore.get().receiptThrough(
                    chatId: state.contact.userId,
                    senderUserId: ownUserId,
                    receiptType: ReceiptType.delivered
                )) ?? 0
                connected = delivered >= state.delivery.lamport
                if !connected { try? await Task.sleep(nanoseconds: 500_000_000) }
            }
        }
    }

    private var statusText: String {
        let displayName = ChatListLogic.contactDisplayName(state.contact)
        if state.awaitingAcceptance {
            return "Waiting for \(displayName) to accept."
        }
        if connected { return "You're connected. \(displayName) has your card too." }
        if state.relayConfigured {
            return "Sending \(displayName) your card through the relay so they can message you back."
        }
        return "Your card will reach \(displayName) next time your phones are near each other. Until then, only you can start the chat."
    }
}

/// The one-time "prove it to yourself" nudge, shown inside the friend-added
/// sheet the first time somebody has a person to try it with. Deliberately a
/// quiet card rather than an alert: it is an invitation, not a warning, and the
/// sheet's own buttons stay the primary action.
struct AirplaneDemoHint: View {
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Try it: turn on airplane mode on both phones, then turn Bluetooth back on — messages still get through.")
                .font(.callout)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button("Got it", action: onDismiss)
                .font(.callout)
                .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .padding(12)
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}
