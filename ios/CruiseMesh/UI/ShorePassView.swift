import SwiftUI
import UIKit

struct ShorePassView: View {
    let initialCard: String?
    @ObservedObject var appModel: AppModel
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @Environment(\.dismiss) private var dismiss

    @State private var input: String
    @State private var configured = RelayConfigStore.load()
    @State private var pending: RelaySetup?
    @State private var pendingUntrusted: RelaySetup?
    @State private var setupTask: Task<Void, Never>?
    @State private var isTesting = false
    @State private var resultMessage: String?
    @State private var resultIsError = false
    @State private var setupCompleted = false
    @State private var savedForLater = false
    @State private var showManualEntry = false
    @State private var showCustom = false
    @State private var unverifiedSetup: RelaySetup?
    @State private var showSetupQR = false
    @State private var showRemoveConfirmation = false
    @State private var showCredentialRefreshConfirmation = false
    @State private var credentialRefreshMessage: LocalizedStringKey?
    @State private var customUrl: String
    @State private var customToken: String
    /// Last health that was an actual answer, so an in-flight re-check keeps
    /// showing the previous verdict instead of flickering the heading.
    @State private var lastVerdict: RelayHealth?

    init(initialCard: String?, appModel: AppModel) {
        self.initialCard = initialCard
        self.appModel = appModel
        let saved = RelayConfigStore.load()
        _input = State(initialValue: initialCard ?? "")
        _customUrl = State(initialValue: saved?.relayUrl ?? "")
        _customToken = State(initialValue: saved?.relayToken ?? "")
    }

    var body: some View {
        Form {
            if isLinkSetup {
                Section {
                    if isTesting || (resultMessage == nil && pending == nil && pendingUntrusted == nil) {
                        Text("Checking your Shore Pass")
                            .font(.title2.weight(.semibold))
                        HStack {
                            ProgressView()
                            Text("This only takes a moment.")
                        }
                    } else if setupCompleted {
                        readyHeading {
                            Text("You’re all set")
                        }
                        Text("Shore Pass is ready on this phone.")
                        Button("Done") { dismiss() }
                            .buttonStyle(.borderedProminent)
                    } else if savedForLater {
                        Text("Setup saved")
                            .font(.title2.weight(.semibold))
                        Text("We’ll finish checking when this phone is online.")
                        Button("Done") { dismiss() }
                            .buttonStyle(.borderedProminent)
                    } else if pending != nil {
                        Text("Confirm Shore Pass change")
                            .font(.title2.weight(.semibold))
                        Text("This link is for a different Shore Pass.")
                            .foregroundStyle(.secondary)
                    } else if pendingUntrusted != nil {
                        Text("Confirm Shore Pass change")
                            .font(.title2.weight(.semibold))
                        Text("This link is for a custom relay.")
                            .foregroundStyle(.secondary)
                    } else if let resultMessage {
                        Text("Shore Pass wasn’t set up")
                            .font(.title2.weight(.semibold))
                        Text(resultMessage)
                            .foregroundStyle(.red)
                        if let initialCard {
                            Button("Try again") { startSetup(initialCard) }
                        }
                    }
                    if let setup = unverifiedSetup {
                        Button("Save and check later") {
                            saveAndCheckLater(setup)
                        }
                    }
                    if resultIsError && !isTesting {
                        Button("Not now", role: .cancel) { dismiss() }
                    }
                }
            } else {
                Section {
                    switch heading {
                    case .ready:
                        readyHeading {
                            Text("Shore Pass is set up")
                        }
                    case .notSetUp:
                        Text("Set up your Shore Pass")
                            .font(.title2.weight(.semibold))
                    case .checking:
                        Text("Checking your Shore Pass")
                            .font(.title2.weight(.semibold))
                    case .configured:
                        Text("Shore Pass is configured")
                            .font(.title2.weight(.semibold))
                    }
                    if configured == nil {
                        Text("Paste the setup card from your purchase email. CruiseMesh will test and save it automatically.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                        // Where a pass comes from, for the person who arrived
                        // here without one: the family share first (one pass
                        // covers everyone), the site second. The paste flow
                        // above stays the primary action for anyone already
                        // holding a card.
                        Text("One Shore Pass covers your whole family. If someone in your family already has one, ask them to share their setup card instead of buying another.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                        Link(
                            "Get a Shore Pass at cruisemesh.app",
                            destination: URL(string: "https://cruisemesh.app/pass/")!
                        )
                        .font(.subheadline)
                        // Who bills for what, before anyone commits to a pass.
                        // Same secondary style as the line above: it is an
                        // answer, not a warning.
                        Text("Shore Pass uses the internet your phone already has. Ship Wi-Fi packages and roaming data are billed by your provider at their normal rates. Everything else in CruiseMesh works without any internet.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    } else {
                        LabeledContent("Status", value: passStatus)
                        // CP2b: plain-language explanation for the structured
                        // delivery states -- what's happening, what happens
                        // next, what to do. Support guidance appears only on
                        // states that do not heal on their own.
                        if let explanation = passStatusExplanation {
                            Text(explanation)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                if configured == nil || showManualEntry {
                    Section("Setup") {
                        Button("Paste and set up", action: pasteAndStart)
                            .buttonStyle(.borderedProminent)
                            .disabled(isTesting)
                        Button(showManualEntry ? "Hide manual entry" : "Enter setup card manually") {
                            showManualEntry.toggle()
                        }
                        if showManualEntry {
                            TextEditor(text: $input)
                                .frame(minHeight: 90)
                                .font(.footnote.monospaced())
                            Button("Check and save") { startSetup(input) }
                                .disabled(
                                    input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
                                    isTesting
                                )
                        }
                        if isTesting {
                            HStack {
                                ProgressView()
                                Text("Checking and saving…")
                            }
                        }
                        if let resultMessage {
                            Text(resultMessage)
                                .font(.caption)
                                .foregroundStyle(resultIsError ? .red : .green)
                        }
                        if let setup = unverifiedSetup {
                            Button("Save and check later") {
                                saveAndCheckLater(setup)
                            }
                        }
                    }
                } else {
                    Section {
                        Button("Use a different Shore Pass") {
                            resultMessage = nil
                            resultIsError = false
                            showManualEntry = true
                        }
                    }
                }

                if let configured {
                    Section("Family phones") {
                        let card = try? makeRelaySetupCard(
                            relayUrl: configured.relayUrl,
                            relayToken: configured.relayToken
                        )
                        if let card, let url = URL(string: "https://cruisemesh.app/r#\(card)") {
                            ShareLink(item: url) {
                                Label("Set up another phone", systemImage: "square.and.arrow.up")
                            }
                            Button {
                                showSetupQR = true
                            } label: {
                                Label("Show setup QR", systemImage: "qrcode")
                            }
                        }
                        Text("Share this only with your family’s phones.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        if relaySetupIsOfficial(relayUrl: configured.relayUrl) {
                            Button("Retire old Shore Pass access") {
                                showCredentialRefreshConfirmation = true
                            }
                            Text("Use this if an older CruiseMesh build shared your family’s full mailbox access in friend cards.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            if let credentialRefreshMessage {
                                Text(credentialRefreshMessage)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Button("Remove Shore Pass setup", role: .destructive) {
                            showRemoveConfirmation = true
                        }
                    }
                }

                Section {
                    DisclosureGroup("Custom relay", isExpanded: $showCustom) {
                        Text("For self-hosted relays and development.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        TextField("Relay URL", text: $customUrl)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        SecureField("Relay token", text: $customToken)
                        Button("Test and save") {
                            do {
                                let card = try makeRelaySetupCard(
                                    relayUrl: customUrl,
                                    relayToken: customToken
                                )
                                testAndSave(try parseRelaySetupText(text: card))
                            } catch {
                                resultIsError = true
                                resultMessage = "Enter a complete HTTPS relay URL and token."
                            }
                        }
                        .disabled(customUrl.isEmpty || customToken.isEmpty || isTesting)
                    }
                }
            }
        }
        .navigationTitle(isLinkSetup ? "Setting up Shore Pass" : "Shore Pass")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Done") { dismiss() }
            }
        }
        .onAppear {
            if let initialCard, !initialCard.isEmpty { startSetup(initialCard) }
        }
        .onDisappear {
            setupTask?.cancel()
        }
        .sheet(isPresented: $showSetupQR) {
            if let configured,
               let card = try? makeRelaySetupCard(
                    relayUrl: configured.relayUrl,
                    relayToken: configured.relayToken
               ) {
                let link = "https://cruisemesh.app/r#\(card)"
                NavigationStack {
                    VStack(spacing: 18) {
                        if let image = QRCodeGenerator.image(from: link, size: 260) {
                            Image(uiImage: image)
                                .interpolation(.none)
                                .resizable()
                                .scaledToFit()
                                .frame(width: 260, height: 260)
                                .padding()
                                .background(RoundedRectangle(cornerRadius: 16).fill(Color.white))
                        }
                        Text("Scan this with the other phone. It configures internet delivery; it does not add a contact.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal)
                        ShareLink(item: link) {
                            Label("Share setup link", systemImage: "square.and.arrow.up")
                        }
                        Spacer()
                    }
                    .padding()
                    .navigationTitle("Family phone setup")
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Done") { showSetupQR = false }
                        }
                    }
                }
            }
        }
        // Cleared on a pass swap first, so a new card never inherits the old
        // card's verdict; then re-latched from whatever health is current.
        .onChange(of: configured) { _ in lastVerdict = nil }
        .onChange(of: connectivity.relay) { health in
            if health.isPassVerdict { lastVerdict = health }
        }
        .onAppear {
            let health = connectivity.relay
            if health.isPassVerdict { lastVerdict = health }
        }
        .confirmationDialog(
            (configured != nil && relaySetupIsOfficial(relayUrl: configured!.relayUrl)) ? "Remove Shore Pass setup?" : "Remove relay setup?",
            isPresented: $showRemoveConfirmation,
            titleVisibility: .visible
        ) {
            Button("Remove setup", role: .destructive) {
                RelayConfigStore.save(relayUrl: "", relayToken: "")
                configured = nil
                MeshConnectivityStatus.shared.setRelayHealth(.noConfig)
                input = ""
                customUrl = ""
                customToken = ""
                resultMessage = nil
                setupCompleted = false
                savedForLater = false
            }
        } message: {
            Text("Queued internet delivery will stop until another Shore Pass or custom relay is set up. Nearby delivery still works.")
        }
        .confirmationDialog(
            "Retire old Shore Pass access?",
            isPresented: $showCredentialRefreshConfirmation,
            titleVisibility: .visible
        ) {
            Button("Retire old access", role: .destructive) {
                if RelayRotationDriver().beginCredentialRefresh() {
                    RelaySyncEvents.requestSync()
                    credentialRefreshMessage = "Access retirement is queued and will finish when CruiseMesh reaches the relay."
                } else {
                    credentialRefreshMessage = "Couldn’t queue access retirement. Try again."
                }
            }
        } message: {
            Text("This replaces the mailbox credential in older friend cards. Messages stay, and your contacts update automatically. Other people using this Shore Pass may need to scan this phone’s setup card again.")
        }
        .alert("Replace Shore Pass?", isPresented: Binding(
            get: { pending != nil },
            set: { if !$0 { pending = nil } }
        )) {
            Button("Keep current pass", role: .cancel) {
                pending = nil
                if isLinkSetup { dismiss() }
            }
            Button("Replace and test") {
                if let setup = pending { testAndSave(setup) }
            }
        } message: {
            if let setup = pending {
                if let configured {
                    Text("Replace \(relayHost(configured.relayUrl)) with \(relayHost(setup.relayUrl))?\n\nYour family's token stays hidden.")
                } else {
                    Text("Host: \(relayHost(setup.relayUrl))\n\nYour family's token stays hidden.")
                }
            }
        }
        .alert("Set up this relay?", isPresented: Binding(
            get: { pendingUntrusted != nil },
            set: { if !$0 { pendingUntrusted = nil } }
        )) {
            Button("Cancel", role: .cancel) {
                pendingUntrusted = nil
                if isLinkSetup { dismiss() }
            }
            Button("Set up and test") {
                if let setup = pendingUntrusted { testAndSave(setup) }
            }
        } message: {
            if let setup = pendingUntrusted {
                Text("Host: \(relayHost(setup.relayUrl))\n\nThis setup card isn’t for the official Shore Pass service. Only continue if you set this relay up yourself.")
            }
        }
    }

    private var isLinkSetup: Bool {
        !(initialCard?.isEmpty ?? true)
    }

    // Verdict-driven, not health-driven: an in-flight re-check must not demote
    // the heading (see ShorePassHeading.of), but any real answer other than
    // OK takes the green check away at once.
    private var heading: ShorePassHeading {
        ShorePassHeading.of(
            connectivity.relay,
            configured: configured != nil,
            lastVerdict: lastVerdict
        )
    }

    private func readyHeading<Content: View>(
        @ViewBuilder content: () -> Content
    ) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .accessibilityHidden(true)
            content()
                .font(.title2.weight(.semibold))
        }
    }

    private func startSetup(_ text: String) {
        do {
            let setup = try parseRelaySetupText(text: text)
            if let configured,
               (configured.relayUrl != setup.relayUrl || configured.relayToken != setup.relayToken) {
                pending = setup
            } else if configured == nil && !relaySetupIsOfficial(relayUrl: setup.relayUrl) {
                pendingUntrusted = setup
            } else {
                testAndSave(setup)
            }
        } catch {
            pending = nil
            setupCompleted = false
            savedForLater = false
            resultIsError = true
            resultMessage = "That setup card is incomplete or invalid. Copy the whole card and try again."
        }
    }

    private func pasteAndStart() {
        input = UIPasteboard.general.string ?? ""
        guard !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            resultIsError = true
            resultMessage = "Copy the setup card from your purchase email first."
            return
        }
        startSetup(input)
    }

    private func saveAndCheckLater(_ setup: RelaySetup) {
        RelayConfigStore.save(
            relayUrl: setup.relayUrl,
            relayToken: setup.relayToken
        )
        configured = RelayConfig(
            relayUrl: setup.relayUrl,
            relayToken: setup.relayToken
        )
        unverifiedSetup = nil
        MeshConnectivityStatus.shared.setRelayHealth(.checking)
        resultIsError = false
        resultMessage = "Setup saved. CruiseMesh will check it when this phone is online."
        setupCompleted = false
        savedForLater = true
        appModel.startMesh()
    }

    private func testAndSave(_ setup: RelaySetup) {
        pending = nil
        pendingUntrusted = nil
        unverifiedSetup = nil
        isTesting = true
        setupCompleted = false
        savedForLater = false
        resultMessage = nil
        setupTask?.cancel()
        setupTask = Task {
            func checkRelay() async -> Result<Void, Error> {
                await Task.detached(priority: .userInitiated) {
                    Result {
                        _ = try RelayClient.syncPresence(
                            config: RelayConfig(relayUrl: setup.relayUrl, relayToken: setup.relayToken),
                            announce: [],
                            query: []
                        )
                    }
                }.value
            }
            var result = await checkRelay()
            // Retry only transport-level failures: HTTP rejections are
            // deterministic, and anything else would fail identically.
            if case .failure(let error) = result, error is URLError {
                try? await Task.sleep(nanoseconds: 750_000_000)
                if Task.isCancelled { return }
                result = await checkRelay()
            }
            // The user dismissed the screen mid-check; saving now would
            // configure the pass behind their back.
            if Task.isCancelled { return }
            await MainActor.run {
                isTesting = false
                switch result {
                case .success:
                    RelayConfigStore.save(relayUrl: setup.relayUrl, relayToken: setup.relayToken)
                    configured = RelayConfig(relayUrl: setup.relayUrl, relayToken: setup.relayToken)
                    showManualEntry = false
                    MeshConnectivityStatus.shared.setRelayHealth(
                        .ok(lastSyncMs: Int64(Date().timeIntervalSince1970 * 1_000))
                    )
                    appModel.startMesh()
                    resultIsError = false
                    resultMessage = "Shore Pass is ready on this phone."
                    setupCompleted = true
                case .failure(let error):
                    resultIsError = true
                    setupCompleted = false
                    if let relay = error as? RelayHTTPError, relay.relayCode == "family_expired" {
                        resultMessage = "This Shore Pass has expired. Renew it, then open the new setup link."
                    } else if let relay = error as? RelayHTTPError, relay.relayCode == "family_suspended" {
                        resultMessage = "This Shore Pass is suspended. Contact support for help."
                    } else if error is RelayHTTPError {
                        resultMessage = "This setup card was rejected. Check the card, or contact support."
                    } else {
                        unverifiedSetup = setup
                        resultMessage = setupFailureMessage(error)
                    }
                }
            }
        }
    }

    private func setupFailureMessage(_ error: Error) -> String {
        guard let urlError = error as? URLError else {
            return "CruiseMesh couldn’t reach Shore Pass. Try again. If it keeps happening, check your VPN or security app."
        }
        switch urlError.code {
        case .timedOut:
            return "Shore Pass took too long to respond. Try again."
        case .cannotFindHost, .dnsLookupFailed:
            return "CruiseMesh couldn’t find the Shore Pass service. Check Private DNS or VPN settings, then try again."
        case .secureConnectionFailed, .serverCertificateUntrusted,
             .serverCertificateHasBadDate, .serverCertificateNotYetValid:
            return "CruiseMesh couldn’t make a secure connection to Shore Pass. Check the phone’s date, VPN, or security app, then try again."
        case .notConnectedToInternet, .networkConnectionLost:
            return "CruiseMesh couldn’t reach Shore Pass. Check the connection or VPN, then try again."
        default:
            return "CruiseMesh couldn’t reach Shore Pass. Try again. If it keeps happening, check your VPN or security app."
        }
    }

    private func relayHost(_ value: String) -> String {
        URL(string: value)?.host ?? value
    }

    private var passStatus: String {
        if isTesting { return "Checking setup…" }
        switch connectivity.relay {
        case .noConfig, .checking:
            return "Checking setup…"
        case .noInternet:
            return "Phone is offline · setup is saved"
        case .deferredRoaming:
            return String(localized: "Waiting for non-roaming internet to avoid roaming charges")
        case .ok(let lastSyncMs):
            return "Ready · checked \(relativeAge(lastSyncMs))"
        case .failing:
            return "Service unavailable · try again later"
        case .expired:
            return "Pass expired · renewal required"
        case .expiredReadOnly:
            return String(localized: "Pass expired · still receiving")
        case .suspended:
            return "Pass suspended · contact support"
        case .tokenRejected:
            return "Setup card rejected"
        case .quotaFull:
            return String(localized: "Storage full · delivery paused")
        case .messageTooLarge:
            return String(localized: "A message is too large to send")
        case .rateLimited:
            return String(localized: "Syncing is slowed · recovers on its own")
        }
    }

    /// CP2b: the longer what/next/what-to-do paragraph for the structured
    /// delivery states, or nil for every state the short status line already
    /// covers. 429 deliberately never mentions support -- it heals on its
    /// own.
    private var passStatusExplanation: String? {
        switch connectivity.relay {
        case .expiredReadOnly:
            return String(localized: "Your Shore Pass has run out. Messages already on their way to you still arrive, and messages still reach your friends whenever you are near each other. New messages can't go out over the internet until you renew the pass.")
        case .quotaFull:
            return String(localized: "The space that holds your family’s waiting messages is full. Delivery resumes as your family’s phones collect their messages, or as older ones expire. If it stays full, contact support@cruisemesh.app.")
        case .messageTooLarge:
            return String(localized: "One of your messages is too large to send over internet delivery. Try a smaller photo or a shorter message. Other messages still deliver normally.")
        case .rateLimited:
            return String(localized: "Syncing is slowed right now. Your messages are still queued and will be delivered. It recovers on its own.")
        default:
            return nil
        }
    }

    private func relativeAge(_ timestampMs: Int64) -> String {
        let minutes = max(0, (Int64(Date().timeIntervalSince1970 * 1_000) - timestampMs) / 60_000)
        if minutes == 0 { return "just now" }
        if minutes < 60 { return "\(minutes)m ago" }
        if minutes < 24 * 60 { return "\(minutes / 60)h ago" }
        return "\(minutes / (24 * 60))d ago"
    }
}
