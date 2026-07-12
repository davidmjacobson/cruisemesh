import SwiftUI

struct FriendsView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    var onDone: () -> Void

    @State private var contacts: [Contact] = []
    @State private var showMyQR = false
    @State private var showScan = false
    @State private var pasteText = ""
    @State private var error: String?

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
                    Button("Import") { importText(pasteText) }
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
                MyQRView(identity: identity, displayName: appModel.displayName)
            }
            .sheet(isPresented: $showScan) {
                QRScannerView { code in
                    showScan = false
                    importText(code)
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
            .onAppear { reload() }
        }
    }

    private func reload() {
        contacts = ((try? AppStore.get().listContacts()) ?? [])
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    private func importText(_ text: String) {
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
            try AppStore.get().upsertContact(contact: contact)
            // Prefer contact's relay if we have none configured.
            if RelayConfigStore.load() == nil,
               let url = card.relayUrl,
               let token = card.relayToken {
                RelayConfigStore.save(relayUrl: url, relayToken: token)
            }
            FriendRequestSender.sendMutualFriendRequest(
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
            reload()
            pasteText = ""
        } catch {
            self.error = error.localizedDescription
        }
    }
}

struct MyQRView: View {
    let identity: Identity
    let displayName: String
    @Environment(\.dismiss) private var dismiss

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
                if let image = QRCodeGenerator.image(from: link, size: 240) {
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
                ShareLink(item: "Add me on CruiseMesh — copy this whole message and paste it in the app:\n\(link)") {
                    Label("Share card text", systemImage: "square.and.arrow.up")
                }
                Button {
                    UIPasteboard.general.string = link
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
