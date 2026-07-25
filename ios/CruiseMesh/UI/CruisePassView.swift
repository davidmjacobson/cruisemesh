import SwiftUI
import UIKit

struct CruisePassView: View {
    let initialCard: String?
    @ObservedObject var appModel: AppModel
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @Environment(\.dismiss) private var dismiss

    @State private var input: String
    @State private var configured = RelayConfigStore.load()
    @State private var pending: RelaySetup?
    @State private var parseError: String?
    @State private var isTesting = false
    @State private var resultMessage: String?
    @State private var resultIsError = false
    @State private var showCustom = false
    @State private var unverifiedSetup: RelaySetup?
    @State private var showSetupQR = false
    @State private var showRemoveConfirmation = false
    @State private var customUrl: String
    @State private var customToken: String

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
            Section {
                Text(configured == nil ? "Set up your Cruise Pass" : "Cruise Pass is configured")
                    .font(.title2.weight(.semibold))
                Text(
                    configured == nil
                        ? "Open the setup link from your purchase email. If it did not open here, paste the setup card below."
                        : "Saved for \(relayHost(configured!.relayUrl)). You can replace it with a new setup card at any time."
                )
                .font(.subheadline)
                .foregroundStyle(.secondary)
                if configured != nil {
                    LabeledContent("Status", value: passStatus)
                }
            }

            Section("Setup card") {
                TextEditor(text: $input)
                    .frame(minHeight: 90)
                    .font(.footnote.monospaced())
                HStack {
                    Button("Paste card") {
                        input = UIPasteboard.general.string ?? ""
                        if !input.isEmpty { review(input) }
                    }
                    Spacer()
                    Button("Review") { review(input) }
                        .buttonStyle(.borderedProminent)
                        .disabled(input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                if let parseError {
                    Text(parseError).font(.caption).foregroundStyle(.red)
                }
                if isTesting {
                    HStack {
                        ProgressView()
                        Text("Checking this setup before saving…")
                    }
                }
                if let resultMessage {
                    Text(resultMessage)
                        .font(.caption)
                        .foregroundStyle(resultIsError ? .red : .green)
                }
                if let setup = unverifiedSetup {
                    Button("Save and check later") {
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
                        resultMessage = "Setup saved. CruiseMesh will verify it when this phone is online."
                        appModel.startMesh()
                    }
                }
            }

            if let configured {
                Section("Household setup") {
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
                    Button("Remove Cruise Pass setup", role: .destructive) {
                        showRemoveConfirmation = true
                    }
                    Text("Anyone with this link can use your family's internet delivery. Share it only with your own phones.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("Each family phone needs this setup. A configured phone with internet can help move the family's queued messages; Cruise Pass does not share that phone's internet connection.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            Section {
                DisclosureGroup("Custom relay", isExpanded: $showCustom) {
                    Text("For self-hosted relays and development. Most people should use the setup card above.")
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
        .navigationTitle("Cruise Pass")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Done") { dismiss() }
            }
        }
        .onAppear {
            if let initialCard, !initialCard.isEmpty { review(initialCard) }
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
        .confirmationDialog(
            "Remove Cruise Pass setup?",
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
            }
        } message: {
            Text("Queued internet delivery will stop until another Cruise Pass or custom relay is set up. Nearby delivery still works.")
        }
        .alert("Use this Cruise Pass?", isPresented: Binding(
            get: { pending != nil },
            set: { if !$0 { pending = nil } }
        )) {
            Button("Cancel", role: .cancel) { pending = nil }
            Button("Test and use") {
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
    }

    private func review(_ text: String) {
        do {
            pending = try parseRelaySetupText(text: text)
            parseError = nil
        } catch {
            pending = nil
            parseError = "That setup card is incomplete or invalid. Copy the whole CMRELAY1 card and try again."
        }
    }

    private func testAndSave(_ setup: RelaySetup) {
        pending = nil
        unverifiedSetup = nil
        isTesting = true
        resultMessage = nil
        Task {
            let result = await Task.detached(priority: .userInitiated) {
                Result {
                    _ = try RelayClient.syncPresence(
                        config: RelayConfig(relayUrl: setup.relayUrl, relayToken: setup.relayToken),
                        announce: [],
                        query: []
                    )
                }
            }.value
            await MainActor.run {
                isTesting = false
                switch result {
                case .success:
                    RelayConfigStore.save(relayUrl: setup.relayUrl, relayToken: setup.relayToken)
                    configured = RelayConfig(relayUrl: setup.relayUrl, relayToken: setup.relayToken)
                    MeshConnectivityStatus.shared.setRelayHealth(
                        .ok(lastSyncMs: Int64(Date().timeIntervalSince1970 * 1_000))
                    )
                    appModel.startMesh()
                    resultIsError = false
                    resultMessage = "Cruise Pass is ready on this phone."
                case .failure(let error):
                    resultIsError = true
                    if let relay = error as? RelayHTTPError, relay.relayCode == "family_expired" {
                        resultMessage = "This Cruise Pass has expired. Renew it, then open the new setup link."
                    } else if let relay = error as? RelayHTTPError, relay.relayCode == "family_suspended" {
                        resultMessage = "This Cruise Pass is suspended. Contact support for help."
                    } else if error is RelayHTTPError {
                        resultMessage = "This setup card was rejected. Check the card, or contact support."
                    } else {
                        unverifiedSetup = setup
                        resultMessage = "CruiseMesh could not check this setup. Retry, or save it and CruiseMesh will check when this phone is online."
                    }
                }
            }
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
        case .ok(let lastSyncMs):
            return "Ready · checked \(relativeAge(lastSyncMs))"
        case .failing:
            return "Service unavailable · try again later"
        case .expired:
            return "Pass expired · renewal required"
        case .suspended:
            return "Pass suspended · contact support"
        case .tokenRejected:
            return "Setup card rejected"
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
