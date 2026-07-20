import SwiftUI

struct FriendPreviewState: Identifiable {
    let contact: Contact
    let warning: String?
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

struct FriendPreviewView: View {
    let state: FriendPreviewState
    let onConfirm: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 18) {
            Text("Add this friend?").font(.title.bold())
            FriendIdentityBlock(contact: state.contact)
            if let warning = state.warning {
                Text(warning).foregroundStyle(.red).font(.callout)
            }
            Button("Add this friend", action: onConfirm).buttonStyle(.borderedProminent)
            Button("Cancel", role: .cancel) { dismiss() }
        }
        .padding(24)
        .presentationDetents([.medium, .large])
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
