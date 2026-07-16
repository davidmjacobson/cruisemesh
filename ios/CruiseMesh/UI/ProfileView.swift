import PhotosUI
import SwiftUI

/// Hosted privacy policy (App Store / Play Console + in-app link).
private let privacyPolicyURL = URL(string: "https://cruisemesh.app/privacy")!

struct ProfileView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    @State private var displayName: String = ""
    @State private var relayUrl: String = ""
    @State private var relayToken: String = ""
    @State private var meshOn = true
    @State private var avatarImage: UIImage?
    @State private var photoItem: PhotosPickerItem?
    @State private var friendsOfFriends = true

    var body: some View {
        NavigationStack {
            Form {
                Section("You") {
                    HStack {
                        Spacer()
                        AvatarView(
                            userId: identity.userId,
                            name: displayName,
                            size: 80,
                            photo: avatarImage
                        )
                        Spacer()
                    }
                    PhotosPicker(selection: $photoItem, matching: .images) {
                        Label("Choose profile photo", systemImage: "photo")
                    }
                    if avatarImage != nil {
                        Button("Remove profile photo", role: .destructive) {
                            ProfilePhotoStore.clear()
                            avatarImage = nil
                            let epoch = ProfileStore.bumpOwnAvatarEpoch()
                            ProfileSyncSender.queueToAllContacts(
                                store: AppStore.get(),
                                identity: identity,
                                displayName: displayName,
                                epoch: epoch
                            )
                        }
                    }
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
                Section("Privacy") {
                    Toggle("Friends of friends", isOn: $friendsOfFriends)
                    Text("Let your CruiseMesh friends introduce you to people they know. Your messages and phone contacts are never shared.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Section("Legal") {
                    Button("Privacy policy") {
                        openURL(privacyPolicyURL)
                    }
                    Text(privacyPolicyURL.absoluteString)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Profile")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        let previousName = appModel.displayName
                        let trimmedName = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
                        ProfileStore.saveDisplayName(trimmedName)
                        appModel.displayName = trimmedName
                        RelayConfigStore.save(relayUrl: relayUrl, relayToken: relayToken)
                        let policyChanged = FriendsOfFriendsStore.isEnabled() != friendsOfFriends
                        if policyChanged {
                            FriendsOfFriendsStore.setEnabled(friendsOfFriends)
                            if !friendsOfFriends { try? AppStore.get().clearFriendSuggestions() }
                        }
                        if trimmedName != previousName {
                            let epoch = ProfileStore.bumpOwnAvatarEpoch()
                            ProfileSyncSender.queueToAllContacts(
                                store: AppStore.get(),
                                identity: identity,
                                displayName: trimmedName,
                                epoch: epoch
                            )
                        }
                        if policyChanged {
                            ProfileSyncSender.queueToAllContacts(
                                store: AppStore.get(),
                                identity: identity,
                                displayName: trimmedName,
                                epoch: ProfileStore.loadOwnAvatarEpoch()
                            )
                            FriendDirectorySender.queueToAllContacts(
                                store: AppStore.get(),
                                identity: identity
                            )
                        }
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
                avatarImage = ProfilePhotoStore.loadAvatarImage()
                friendsOfFriends = FriendsOfFriendsStore.isEnabled()
            }
            .onChange(of: photoItem) { item in
                guard let item else { return }
                Task {
                    guard let data = try? await item.loadTransferable(type: Data.self),
                          let image = UIImage(data: data),
                          let saved = ProfilePhotoStore.save(image: image) else { return }
                    await MainActor.run {
                        avatarImage = saved
                        let epoch = ProfileStore.bumpOwnAvatarEpoch()
                        ProfileSyncSender.queueToAllContacts(
                            store: AppStore.get(),
                            identity: identity,
                            displayName: displayName,
                            epoch: epoch
                        )
                    }
                }
            }
        }
    }
}
