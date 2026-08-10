import SwiftUI

/// One contact wrapped for `.sheet(item:)`, which needs an `Identifiable`.
struct ShareContactState: Identifiable {
    let contact: Contact
    var id: String { UserIdHex.encode(contact.userId) }
}

/// Hand one specific contact's card to somebody standing in front of you
/// (specs/share-contact.md). A displayed code and nothing else: no share sheet,
/// no copy button, no link. Putting another person's keys and their family's
/// mailbox token into a group chat is a different act wearing the same button,
/// so the affordance simply does not exist here (decision 2).
struct ShareContactView: View {
    let contact: Contact
    let identity: Identity
    @Environment(\.dismiss) private var dismiss
    @State private var code: String?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    if let code {
                        Text(contact.name)
                            .font(.title2.bold())
                            .multilineTextAlignment(.center)
                        Text(formatUserId(userId: contact.userId))
                            .font(.body.monospaced())
                            .foregroundStyle(.secondary)
                        if let image = QRCodeGenerator.image(from: code, size: 280) {
                            Image(uiImage: image)
                                .interpolation(.none)
                                .resizable()
                                .scaledToFit()
                                .frame(width: 280, height: 280)
                                .padding()
                                .background(RoundedRectangle(cornerRadius: 16).fill(Color.white))
                        }
                        Text("Anyone with this code can ask to connect with \(contact.name). \(contact.name) chooses whether to accept. The code stops working in 7 days.")
                            .font(.callout)
                            .multilineTextAlignment(.center)
                    } else {
                        // Their switch governs manual sharing too: turning off
                        // friends of friends means "do not hand me around", and
                        // it would be incoherent for that to stop the automatic
                        // introductions and permit this one (decision 4).
                        Text("\(contact.name) has turned off being introduced to others.")
                            .font(.body)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                }
                .padding(24)
            }
            .navigationTitle("Share contact")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        // Built once, not on every redraw: each call signs a fresh card with a
        // fresh issue time, and the code on screen must not keep changing under
        // the camera pointed at it.
        .onAppear { code = makeCode() }
    }

    private func makeCode() -> String? {
        guard let policy = try? AppStore.get().getContactDiscoveryPolicy(userId: contact.userId),
              policy.enabled else { return nil }
        // Decision 8: the relay fields go out exactly as stored for them. Never
        // this phone's own token, and never its own relay config filling in a
        // gap -- that would hand out a credential they never had.
        let card = FriendCard(
            name: contact.name,
            signPk: contact.signPk,
            agreePk: contact.agreePk,
            relayUrl: contact.relayUrl,
            relayToken: contact.relayToken,
            // No primary self-signature when re-sharing: the sharer never holds
            // the contact's signing key. Integrity of a shared card comes from
            // the sharer's own SharedFriendCard signature instead.
            signature: nil
        )
        guard let shared = try? createSharedFriendCard(
            sharer: identity,
            card: card,
            sharedPolicyRevision: policy.revision,
            nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ) else { return nil }
        return try? makeSharedContactCode(shared: shared)
    }
}
