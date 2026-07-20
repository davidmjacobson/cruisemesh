import Combine
import SwiftUI

struct FriendsView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    var initialToken: String? = nil
    var onDone: () -> Void

    @State private var contacts: [Contact] = []
    @State private var showMyQR = false
    @State private var showScan = false
    @State private var pasteText = ""
    @State private var error: String?
    @State private var preview: FriendPreviewState?
    @State private var added: FriendAddedState?
    @State private var chatContact: Contact?
    @State private var suggestions: [FriendSuggestion] = []
    @State private var showAddAllConfirmation = false

    private var groupedSuggestions: [(Data, [FriendSuggestion])] {
        Dictionary(grouping: suggestions, by: { $0.candidate.userId })
            .map { ($0.key, $0.value) }
            .sorted { $0.1[0].candidate.name.localizedCaseInsensitiveCompare($1.1[0].candidate.name) == .orderedAscending }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    if !FriendsOfFriendsStore.isEnabled() {
                        Text("Friends-of-friends introductions are off in Profile.")
                            .foregroundStyle(.secondary)
                    } else if groupedSuggestions.isEmpty {
                        Text("Suggestions appear after your friends' phones sync.")
                            .foregroundStyle(.secondary)
                    } else {
                        if groupedSuggestions.filter({ $0.1[0].state == 0 }).count > 1 {
                            Button("Add all (\(groupedSuggestions.filter { $0.1[0].state == 0 }.count))") {
                                showAddAllConfirmation = true
                            }
                        }
                        ForEach(groupedSuggestions.indices, id: \.self) { index in
                            let sources = groupedSuggestions[index].1
                            let suggestion = sources[0]
                            let mutualNames = sources.compactMap {
                                (try? AppStore.get().getContact(userId: $0.introducerUserId))?.name
                            }
                            HStack {
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(suggestion.candidate.name)
                                    Text("Through \(mutualNames.isEmpty ? "a mutual friend" : mutualNames.joined(separator: ", "))")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                Spacer()
                                Button(suggestion.state == 1 ? "Requested" : "Add") {
                                    request(suggestion)
                                }
                                .disabled(suggestion.state != 0)
                                Button(role: .destructive) {
                                    try? AppStore.get().setFriendSuggestionState(
                                        candidateUserId: suggestion.candidate.userId,
                                        state: 2
                                    )
                                    reload()
                                } label: {
                                    Image(systemName: "xmark")
                                }
                                .buttonStyle(.borderless)
                            }
                        }
                    }
                } header: {
                    Text("Friends of friends")
                }
                Section("Add directly") {
                    Button { showScan = true } label: {
                        Label("Scan friend QR", systemImage: "qrcode.viewfinder")
                    }
                    Button { showMyQR = true } label: {
                        Label("My friend card", systemImage: "qrcode")
                    }
                }
                Section("Paste friend card") {
                    TextField("Friend card", text: $pasteText, axis: .vertical)
                        .lineLimit(3...8)
                    HStack {
                        Button("Paste") { pasteText = UIPasteboard.general.string ?? "" }
                        Spacer()
                        Button("Preview friend") { previewText(pasteText) }
                    }
                }
                Section("Friends") {
                    if contacts.isEmpty {
                        VStack(spacing: 8) {
                            Image(systemName: "person.crop.circle.badge.plus")
                                .font(.title)
                                .foregroundStyle(.secondary)
                            Text("No friends yet")
                                .font(.headline)
                            Text("Scan or paste a friend card to get started.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .multilineTextAlignment(.center)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 12)
                    }
                    ForEach(contacts, id: \.userId) { contact in
                        NavigationLink {
                            ChatView(contact: contact, identity: identity)
                        } label: {
                            HStack {
                                AvatarView(
                                    userId: contact.userId,
                                    name: contact.name,
                                    photo: (try? AppStore.get().contactAvatar(userId: contact.userId))
                                        .flatMap { UIImage(data: $0) }
                                )
                                VStack(alignment: .leading) {
                                    Text(ChatListLogic.displayNameOrId(
                                        name: contact.name,
                                        displayId: formatUserId(userId: contact.userId)
                                    ))
                                    Text(formatUserId(userId: contact.userId))
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle("Friends")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done", action: onDone)
                }
            }
            .sheet(isPresented: $showMyQR) {
                MyQRView(identity: identity, displayName: appModel.displayName) { contact in
                    showMyQR = false
                    chatContact = contact
                }
            }
            .sheet(isPresented: $showScan) {
                QRScannerView { code in
                    showScan = false
                    previewText(code)
                }
            }
            .sheet(item: $preview) { state in
                FriendPreviewView(state: state) { confirm(state.contact) }
            }
            .sheet(item: $added) { state in
                FriendConfirmationView(
                    state: state,
                    ownUserId: identity.userId,
                    onSayHi: {
                        added = nil
                        chatContact = state.contact
                    },
                    onAddAnother: {
                        added = nil
                        DispatchQueue.main.async { showScan = true }
                    },
                    onDone: { added = nil }
                )
            }
            .navigationDestination(isPresented: Binding(
                get: { chatContact != nil },
                set: { if !$0 { chatContact = nil } }
            )) {
                if let contact = chatContact {
                    ChatView(contact: contact, identity: identity)
                }
            }
            .alert("Import failed", isPresented: Binding(
                get: { error != nil },
                set: { if !$0 { error = nil } }
            )) {
                Button("OK", role: .cancel) { error = nil }
            } message: {
                Text(error ?? "")
            }
            .confirmationDialog(
                "Add all suggested friends?",
                isPresented: $showAddAllConfirmation,
                titleVisibility: .visible
            ) {
                Button("Add all") {
                    groupedSuggestions.map { $0.1[0] }.filter { $0.state == 0 }.forEach(request)
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("CruiseMesh will request each connection through the mutual friends shown in the list.")
            }
            .onAppear {
                reload()
                if let initialToken, !initialToken.isEmpty {
                    pasteText = initialToken
                    previewText(initialToken)
                }
            }
            .onReceive(ChatEvents.subject.receive(on: DispatchQueue.main)) { _ in reload() }
        }
    }

    private func reload() {
        contacts = ((try? AppStore.get().listContacts()) ?? [])
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        suggestions = FriendsOfFriendsStore.isEnabled()
            ? ((try? AppStore.get().listFriendSuggestions(
                nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
              )) ?? [])
            : []
    }

    private func request(_ suggestion: FriendSuggestion) {
        _ = FriendDirectorySender.requestSuggestedFriend(
            store: AppStore.get(),
            identity: identity,
            displayName: appModel.displayName,
            suggestion: suggestion
        )
        reload()
    }

    private func previewText(_ text: String) {
        do {
            let card = try parseFriendText(text: text)
            let userId = friendCardUserId(card: card)
            guard userId != identity.userId else {
                error = "That is your own card"
                return
            }
            let contact = Contact(
                userId: userId,
                name: card.name,
                signPk: card.signPk,
                agreePk: card.agreePk,
                relayUrl: card.relayUrl,
                relayToken: card.relayToken
            )
            let collision = ((try? AppStore.get().listContacts()) ?? []).first {
                $0.name.caseInsensitiveCompare(contact.name) == .orderedSame && $0.userId != contact.userId
            }
            let warning = collision == nil ? nil :
                "You already have a \(contact.name); this card has different security keys. Compare the fingerprint words before adding it."
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            preview = FriendPreviewState(contact: contact, warning: warning)
        } catch {
            self.error = text.contains("CMFRIEND")
                ? "That looks like a friend card but part of it is missing. Copy the whole message and try again."
                : "Not a CruiseMesh friend card"
        }
    }

    private func confirm(_ candidate: Contact) {
        do {
            let contact = try AppStore.get().upsertImportedContact(contact: candidate)
            try? AppStore.get().upsertContactProvenance(provenance: ContactProvenance(
                userId: contact.userId,
                source: 0,
                introducerUserId: nil,
                introducedAtMs: Int64(Date().timeIntervalSince1970 * 1_000)
            ))
            try? AppStore.get().removeFriendSuggestion(candidateUserId: contact.userId)
            if RelayConfigStore.load() == nil,
               let url = contact.relayUrl,
               let token = contact.relayToken {
                RelayConfigStore.save(relayUrl: url, relayToken: token)
            }
            let delivery = FriendRequestSender.sendMutualFriendRequest(
                store: AppStore.get(),
                identity: identity,
                contact: contact,
                displayName: appModel.displayName
            )
            ProfileSyncSender.queueToContact(
                store: AppStore.get(),
                identity: identity,
                contact: contact,
                displayName: appModel.displayName,
                epoch: ProfileStore.loadOwnAvatarEpoch()
            )
            FriendDirectorySender.queueToAllContacts(store: AppStore.get(), identity: identity)
            reload()
            pasteText = ""
            preview = nil
            DispatchQueue.main.async {
                added = FriendAddedState(
                    contact: contact,
                    delivery: delivery,
                    relayConfigured: RelayConfigStore.load() != nil
                )
            }
        } catch {
            self.error = error.localizedDescription
        }
    }
}

struct MyQRView: View {
    let identity: Identity
    let displayName: String
    let onSayHi: (Contact) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var connectedFriend: FriendAddedState?

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                let json = try? makeFriendCard(
                    name: displayName.isEmpty ? "Friend" : displayName,
                    identity: identity,
                    relayUrl: RelayConfigStore.load()?.relayUrl,
                    relayToken: RelayConfigStore.load()?.relayToken
                )
                let link = json.flatMap { try? makeFriendLink(cardJson: $0) }
                let appLink = link.map { "https://cruisemesh.app/f#\($0)" }
                if let appLink {
                    if let image = QRCodeGenerator.image(from: appLink, size: 280) {
                        Image(uiImage: image)
                            .interpolation(.none)
                            .resizable()
                            .scaledToFit()
                            .frame(width: 280, height: 280)
                            .padding()
                            .background(RoundedRectangle(cornerRadius: 16).fill(Color.white))
                    }
                    ShareLink(item: "Add me on CruiseMesh: \(appLink)") {
                        Label("Share card text", systemImage: "square.and.arrow.up")
                    }
                } else {
                    Text("Shorten your name or relay settings to create a friend card.")
                        .font(.body)
                        .foregroundStyle(.red)
                        .multilineTextAlignment(.center)
                }
                Text(formatUserId(userId: identity.userId))
                    .font(.footnote.monospaced())
                Text(fingerprintWords(userId: identity.userId).joined(separator: " "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
                if let appLink {
                    Button {
                        UIPasteboard.general.string = appLink
                    } label: {
                        Label("Copy link", systemImage: "doc.on.doc")
                    }
                }
                Spacer()
            }
            .padding()
            .navigationTitle("My friend card")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .onReceive(FriendImportEvents.subject.receive(on: DispatchQueue.main)) { event in
            guard event.directBluetooth else { return }
            connectedFriend = FriendAddedState(
                contact: event.contact,
                delivery: FriendRequestDelivery(reachedDirectly: true, lamport: 0),
                relayConfigured: RelayConfigStore.load() != nil
            )
        }
        .sheet(item: $connectedFriend) { state in
            FriendConfirmationView(
                state: state,
                ownUserId: identity.userId,
                onSayHi: {
                    connectedFriend = nil
                    dismiss()
                    onSayHi(state.contact)
                },
                onAddAnother: nil,
                onDone: { connectedFriend = nil }
            )
        }
    }
}

enum QRCodeGenerator {
    static func image(from string: String, size: CGFloat) -> UIImage? {
        let data = Data(string.utf8)
        guard let filter = CIFilter(name: "CIQRCodeGenerator") else { return nil }
        filter.setValue(data, forKey: "inputMessage")
        // Level L (7% recovery) is plenty for screen-to-screen scanning and
        // keeps the module count -- and so the density -- as low as possible,
        // matching zxing's default on Android (T12).
        filter.setValue("L", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage else { return nil }
        let scale = size / output.extent.width
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let context = CIContext()
        guard let cg = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return UIImage(cgImage: cg)
    }
}
