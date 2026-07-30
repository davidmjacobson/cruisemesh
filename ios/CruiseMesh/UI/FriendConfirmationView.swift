import SwiftUI

struct FriendPreviewState: Identifiable {
    let contact: Contact
    /// How this card relates to contacts already saved, decided in core so both
    /// shells agree (`friend_card_match`).
    let match: FriendCardMatch
    var id: String { UserIdHex.encode(contact.userId) }
}

struct FriendAddedState: Identifiable {
    let contact: Contact
    let delivery: FriendRequestDelivery
    let relayConfigured: Bool
    var id: String { UserIdHex.encode(contact.userId) }
}

struct FriendIdentityBlock: View {
    let contact: Contact

    var body: some View {
        VStack(spacing: 10) {
            AvatarView(
                userId: contact.userId,
                name: contact.name,
                size: 72,
                photo: (try? AppStore.get().contactAvatar(userId: contact.userId)).flatMap { UIImage(data: $0) }
            )
            Text(contact.name).font(.title2.bold())
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
        _connected = State(initialValue: state.delivery.lamport == 0 && state.delivery.reachedDirectly)
    }

    var body: some View {
        VStack(spacing: 18) {
            Text(connected ? "You're connected" : "Friend added").font(.title.bold())
            FriendIdentityBlock(contact: state.contact)
            Label(statusText, systemImage: connected ? "checkmark.circle.fill" : "clock.arrow.circlepath")
                .font(.callout)
                .foregroundStyle(connected ? Color.accentColor : .secondary)
            Button("Say hi", action: onSayHi).buttonStyle(.borderedProminent)
            if let onAddAnother { Button("Add another", action: onAddAnother) }
            Button("Done", action: onDone)
        }
        .padding(24)
        .presentationDetents([.medium, .large])
        .interactiveDismissDisabled(false)
        .task(id: state.id) {
            while !connected && state.delivery.lamport > 0 {
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
        if connected { return "You're connected. \(state.contact.name) has your card too." }
        if state.relayConfigured {
            return "Sending \(state.contact.name) your card through the relay so they can message you back."
        }
        return "Your card will reach \(state.contact.name) next time your phones are near each other. Until then, only you can start the chat."
    }
}
