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

    var body: some View {
        NavigationStack {
            List {
                Section {
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
            .onAppear {
                reload()
                if let initialToken, !initialToken.isEmpty {
                    pasteText = initialToken
                    previewText(initialToken)
                }
            }
        }
    }

    private func reload() {
        contacts = ((try? AppStore.get().listContacts()) ?? [])
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    private func previewText(_ text: String) {
        do {
            let card = try parseFriendText(text: extractFriendToken(text))
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
            self.error = text.contains("CMFRIEND1:")
                ? "That looks like a friend card but part of it is missing. Copy the whole message and try again."
                : "Not a CruiseMesh friend card"
        }
    }

    private func confirm(_ candidate: Contact) {
        do {
            try AppStore.get().upsertContact(contact: candidate)
            if RelayConfigStore.load() == nil,
               let url = candidate.relayUrl,
               let token = candidate.relayToken {
                RelayConfigStore.save(relayUrl: url, relayToken: token)
            }
            let delivery = FriendRequestSender.sendMutualFriendRequest(
                store: AppStore.get(),
                identity: identity,
                contact: candidate,
                displayName: appModel.displayName
            )
            ProfileSyncSender.queueToContact(
                store: AppStore.get(),
                identity: identity,
                contact: candidate,
                displayName: appModel.displayName,
                epoch: ProfileStore.loadOwnAvatarEpoch()
            )
            reload()
            pasteText = ""
            preview = nil
            DispatchQueue.main.async {
                added = FriendAddedState(
                    contact: candidate,
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
                let json = makeFriendCard(
                    name: displayName.isEmpty ? "Friend" : displayName,
                    identity: identity,
                    relayUrl: RelayConfigStore.load()?.relayUrl,
                    relayToken: RelayConfigStore.load()?.relayToken
                )
                let link = makeFriendLink(cardJson: json)
                let appLink = "https://cruisemesh.app/f#\(link)"
                if let image = QRCodeGenerator.image(from: appLink, size: 240) {
                    Image(uiImage: image)
                        .interpolation(.none)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 240, height: 240)
                        .padding()
                        .background(RoundedRectangle(cornerRadius: 16).fill(Color.white))
                }
                Text(formatUserId(userId: identity.userId))
                    .font(.footnote.monospaced())
                Text(fingerprintWords(userId: identity.userId).joined(separator: " "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
                ShareLink(item: "Add me on CruiseMesh: \(appLink)") {
                    Label("Share card text", systemImage: "square.and.arrow.up")
                }
                Button {
                    UIPasteboard.general.string = appLink
                } label: {
                    Label("Copy link", systemImage: "doc.on.doc")
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
        filter.setValue("M", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage else { return nil }
        let scale = size / output.extent.width
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let context = CIContext()
        guard let cg = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return UIImage(cgImage: cg)
    }
}
