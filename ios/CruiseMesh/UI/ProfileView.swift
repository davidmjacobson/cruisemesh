import SwiftUI

struct ProfileView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @Environment(\.dismiss) private var dismiss

    @State private var displayName: String = ""
    @State private var relayUrl: String = ""
    @State private var relayToken: String = ""
    @State private var meshOn = true

    var body: some View {
        NavigationStack {
            Form {
                Section("You") {
                    TextField("Display name", text: $displayName)
                    LabeledContent("User ID", value: formatUserId(userId: identity.userId))
                    LabeledContent(
                        "Fingerprint",
                        value: fingerprintWords(userId: identity.userId).joined(separator: " ")
                    )
                    Text("Read these words aloud with your friend to verify keys.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Section("Relay (optional)") {
                    TextField("Relay URL", text: $relayUrl)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    SecureField("Family token", text: $relayToken)
                    Text("When any family phone has internet, queued messages flush through this mailbox.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Section("Mesh") {
                    Toggle("Mesh running", isOn: $meshOn)
                        .onChange(of: meshOn) { on in
                            if on { appModel.startMesh() } else { appModel.stopMesh() }
                        }
                    LabeledContent("Status", value: runtime.pillText)
                }
            }
            .navigationTitle("Profile")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        let trimmedName = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
                        ProfileStore.saveDisplayName(trimmedName)
                        appModel.displayName = trimmedName
                        RelayConfigStore.save(relayUrl: relayUrl, relayToken: relayToken)
                        dismiss()
                    }
                }
            }
            .onAppear {
                displayName = appModel.displayName
                if let cfg = RelayConfigStore.load() {
                    relayUrl = cfg.relayUrl
                    relayToken = cfg.relayToken
                }
                meshOn = appModel.meshEnabled
            }
        }
    }
}
