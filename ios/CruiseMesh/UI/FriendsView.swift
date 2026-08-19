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
    /// Shared-card requests waiting on an answer from this phone, and the
    /// requests this phone sent from somebody else's shared code
    /// (specs/share-contact.md).
    @State private var pendingShared: [PendingSharedRequest] = []
    @State private var outgoingShared: [OutgoingSharedRequest] = []
    @State private var pendingSharedSheet: PendingSharedRequestState?
    @State private var shareContact: ShareContactState?
    @FocusState private var pasteFocused: Bool

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
                // A request that is never answered must not be visible only as
                // a notification that has since been swiped away.
                if !pendingShared.isEmpty {
                    Section("Waiting to connect") {
                        ForEach(pendingShared, id: \.requesterUserId) { request in
                            Button {
                                openPendingShared(request)
                            } label: {
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(request.name)
                                    Text("Shared by \(sharerLabel(for: request.sharerUserId))")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                            .tint(.primary)
                        }
                    }
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
                        .accessibilityIdentifier("friends.card-input")
                        .lineLimit(3...8)
                        .focused($pasteFocused)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    HStack {
                        Button("Paste") {
                            pasteText = UIPasteboard.general.string ?? ""
                            // Focus only when paste actually filled text so an
                            // empty clipboard does not force the keyboard open.
                            if !pasteText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                                pasteFocused = true
                            }
                        }
                        Spacer()
                        Button("Preview friend") { submitPaste() }
                    }
                    // XCUITest cannot reliably typeText into this multi-line
                    // field on the headless CI simulator (no keyboard focus).
                    // Seed + focus through the same bindings the human path
                    // uses, without UIPasteboard (which can hang the app idle).
                    // Text(verbatim:) so the localization gate ignores this
                    // test-only control.
                    if UITestConfiguration.isEnabled {
                        Button {
                            pasteText = "not-a-real-card"
                            pasteFocused = true
                        } label: {
                            Text(verbatim: "UITest seed friend card")
                        }
                        .accessibilityIdentifier("friends.uitest-seed-card")
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
                        let displayName = ChatListLogic.contactDisplayName(contact)
                        NavigationLink {
                            ChatView(contact: contact, identity: identity)
                        } label: {
                            HStack {
                                AvatarView(
                                    userId: contact.userId,
                                    name: displayName,
                                    photo: (try? AppStore.get().contactAvatar(userId: contact.userId))
                                        .flatMap { UIImage(data: $0) }
                                )
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(displayName)
                                    if let waiting = waitingText(for: contact) {
                                        Text(waiting)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                            }
                        }
                        .contextMenu {
                            Button {
                                shareContact = ShareContactState(contact: contact)
                            } label: {
                                Label("Share contact", systemImage: "qrcode")
                            }
                        }
                    }
                }
            }
            .navigationTitle("Friends")
            .scrollDismissesKeyboard(.interactively)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done", action: onDone)
                }
                // The keyboard covers the "Preview friend" button under the
                // paste field, so the only way forward used to be dismissing
                // the keyboard first. Put the same action above the keyboard.
                ToolbarItemGroup(placement: .keyboard) {
                    Spacer()
                    Button("Preview friend") { submitPaste() }
                        .accessibilityIdentifier("friends.preview-keyboard")
                        .disabled(pasteText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
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
                    previewText(code, scanned: true)
                }
            }
            .sheet(item: $preview) { state in
                FriendPreviewView(state: state) { confirm(state) }
            }
            .sheet(item: $shareContact) { state in
                ShareContactView(contact: state.contact, identity: identity)
            }
            .sheet(item: $pendingSharedSheet) { state in
                PendingSharedRequestView(
                    state: state,
                    onConnect: { acceptPendingShared(state.request) },
                    onNotNow: { dismissPendingShared(state.request) },
                    onNeverAsk: { suppressPendingShared(state.request) }
                )
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
            .accessibilityIdentifier("screen.friends")
            .onReceive(ChatEvents.subject.receive(on: DispatchQueue.main)) { _ in reload() }
        }
    }

    private func reload() {
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        contacts = ((try? AppStore.get().listContacts()) ?? [])
            .sorted {
                coreContactDisplayName(contact: $0).localizedCaseInsensitiveCompare(
                    coreContactDisplayName(contact: $1)
                ) == .orderedAscending
            }
        suggestions = FriendsOfFriendsStore.isEnabled()
            ? ((try? AppStore.get().listFriendSuggestions(nowMs: now)) ?? [])
            : []
        // Expired rows are swept by the store on read, so this list never shows
        // a request whose card has already died.
        pendingShared = (try? AppStore.get().listPendingSharedRequests(nowMs: now)) ?? []
        outgoingShared = (try? AppStore.get().listOutgoingSharedRequests()) ?? []
    }

    /// The sharer's name when we still have them, their formatted UserID when
    /// we do not — never nothing, so "Shared by" is always a real answer.
    private func sharerLabel(for userId: Data) -> String {
        let name = (try? AppStore.get().getContact(userId: userId))
            .map { coreContactDisplayName(contact: $0) }
        if let name, !name.isEmpty { return name }
        return formatUserId(userId: userId)
    }

    /// Somebody added from a shared code has not agreed to anything yet, and
    /// every rejection path is silent by design — so say what is true rather
    /// than letting the row imply a connection, and once the card has expired
    /// say something they can act on.
    private func waitingText(for contact: Contact) -> String? {
        guard let row = outgoingShared.first(where: { $0.candidateUserId == contact.userId }) else {
            return nil
        }
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let displayName = ChatListLogic.contactDisplayName(contact)
        return row.expiresAtMs > now
            ? "Waiting for \(displayName) to accept."
            : "\(displayName) didn't respond. Ask them to scan your code directly."
    }

    private func openPendingShared(_ request: PendingSharedRequest) {
        let dismissal = (try? AppStore.get().getSharedRequestDismissal(
            requesterUserId: request.requesterUserId
        )) ?? nil
        pendingSharedSheet = PendingSharedRequestState(
            request: request,
            sharerLabel: sharerLabel(for: request.sharerUserId),
            dismissalCount: dismissal?.count ?? 0
        )
    }

    private func acceptPendingShared(_ request: PendingSharedRequest) {
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let candidate = Contact(
            userId: request.requesterUserId,
            name: request.name,
            signPk: request.signPk,
            agreePk: request.agreePk,
            relayUrl: request.relayUrl,
            relayToken: request.relayToken
        )
        guard let contact = try? AppStore.get().upsertImportedContact(contact: candidate) else {
            error = "Could not save this contact. Try again."
            return
        }
        try? AppStore.get().upsertContactProvenance(provenance: ContactProvenance(
            userId: contact.userId,
            source: 2,
            introducerUserId: request.sharerUserId,
            introducedAtMs: now,
            addedNearby: MeshConnectivityStatus.shared.nearbyPeerIds.contains(contact.userId)
        ))
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
        FriendDirectorySender.queueToAllContacts(store: AppStore.get(), identity: identity)
        try? AppStore.get().deletePendingSharedRequest(requesterUserId: request.requesterUserId)
        pendingSharedSheet = nil
        reload()
    }

    /// Not now: they may ask again, but the count survives the row it came from,
    /// so the second ask is the one that offers a way out for good.
    private func dismissPendingShared(_ request: PendingSharedRequest) {
        try? AppStore.get().deletePendingSharedRequest(requesterUserId: request.requesterUserId)
        _ = try? AppStore.get().recordSharedRequestDismissal(requesterUserId: request.requesterUserId)
        pendingSharedSheet = nil
        reload()
    }

    /// A quiet local tombstone. Nobody is told, and scanning that person's own
    /// code later clears it.
    private func suppressPendingShared(_ request: PendingSharedRequest) {
        try? AppStore.get().suppressSharedRequests(requesterUserId: request.requesterUserId)
        try? AppStore.get().deletePendingSharedRequest(requesterUserId: request.requesterUserId)
        pendingSharedSheet = nil
        reload()
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

    /// Drop the keyboard before previewing so the sheet is not fighting it.
    /// A pasted card is never `scanned`: it says nothing about where its owner is.
    private func submitPaste() {
        pasteFocused = false
        previewText(pasteText)
    }

    private func previewText(_ text: String, scanned: Bool = false) {
        let imported: FriendImport
        do {
            imported = try parseFriendImport(text: text)
        } catch {
            self.error = friendImportFailureText(error, text: text)
            return
        }
        let card: FriendCard
        var shared: SharedFriendCard? = nil
        switch imported {
        case let .direct(directCard):
            card = directCard
        case let .shared(sharedCard):
            // An expired share is the common case, not a malformed one, so it
            // gets its own literal answer instead of a parse failure.
            guard !sharedCardExpired(
                shared: sharedCard,
                nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
            ) else {
                error = "This code has expired. Ask for a new one."
                return
            }
            card = sharedCard.card
            shared = sharedCard
        }
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
        let match = friendCardMatch(
            candidate: contact,
            existing: (try? AppStore.get().listContacts()) ?? []
        )
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        preview = FriendPreviewState(
            contact: contact,
            match: match,
            // Pointing the camera at a shared code is co-presence with the
            // sharer, not with the person on the card, so it is never `scanned`.
            scanned: scanned && shared == nil,
            shared: shared,
            sharedByLabel: shared.map { sharerLabel(for: $0.sharerUserId) },
            legacyUnverified: !scanned && shared == nil && card.signature == nil
        )
    }

    private func confirm(_ state: FriendPreviewState) {
        let candidate = state.contact
        let scanned = state.scanned
        let shared = state.shared
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        do {
            let contact = try AppStore.get().upsertImportedContact(contact: candidate)
            // Pointing a camera at their screen means we were standing
            // together; a pasted card may equally have been forwarded from an
            // aeroplane, so only a live link to them counts as having met.
            try? AppStore.get().upsertContactProvenance(provenance: ContactProvenance(
                userId: contact.userId,
                source: shared == nil ? 0 : 2,
                introducerUserId: shared?.sharerUserId,
                introducedAtMs: now,
                addedNearby: scanned || MeshConnectivityStatus.shared.nearbyPeerIds.contains(contact.userId)
            ))
            if let shared {
                // Their phone will hold this request until they answer it, and
                // may never answer at all, so remember what we are waiting on.
                try? AppStore.get().upsertOutgoingSharedRequest(request: OutgoingSharedRequest(
                    candidateUserId: contact.userId,
                    expiresAtMs: shared.expiresAtMs,
                    sentAtMs: now
                ))
            } else {
                // Scanning somebody's own code is the escape hatch: it clears
                // any "Don't ask again" tombstone we once wrote for them.
                try? AppStore.get().clearSharedRequestDismissal(requesterUserId: contact.userId)
            }
            try? AppStore.get().removeFriendSuggestion(candidateUserId: contact.userId)
            // CP4: post-CP4 friend cards carry a post-only deposit token —
            // fine for the contact record (sends resolve through it), never
            // for this phone's OWN config: adopting it would leave the phone
            // unable to fetch its own mail (403 deposit_only on every poll).
            // Own config comes from the member-scoped Shore Pass setup card.
            if RelayConfigStore.load() == nil,
               let url = contact.relayUrl,
               let token = contact.relayToken,
               !relayTokenIsDeposit(token: token) {
                RelayConfigStore.save(relayUrl: url, relayToken: token)
            }
            let delivery = FriendRequestSender.sendMutualFriendRequest(
                store: AppStore.get(),
                identity: identity,
                contact: contact,
                displayName: appModel.displayName,
                shared: shared
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
                    relayConfigured: RelayConfigStore.load() != nil,
                    awaitingAcceptance: shared != nil
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
                    Text("Shorten your name or internet delivery settings to create a friend card.")
                        .font(.body)
                        .foregroundStyle(.red)
                        .multilineTextAlignment(.center)
                }
                // Safety words moved off the card to a "Verify my identity" row
                // in Profile; the friend verifies via "Verify contact" (T10).
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
