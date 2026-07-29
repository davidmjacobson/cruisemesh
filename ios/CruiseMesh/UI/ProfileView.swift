import PhotosUI
import SwiftUI

struct ProfileView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @Environment(\.dismiss) private var dismiss

    @State private var displayName = ""
    @State private var avatarImage: UIImage?
    @State private var photoItem: PhotosPickerItem?
    @State private var showMyCard = false

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
                            syncProfile(epoch: ProfileStore.bumpOwnAvatarEpoch())
                        }
                    }
                    // Labelled, not a bare TextField: SwiftUI treats the
                    // TextField string as a *placeholder*, which disappears the
                    // moment the field has a value. A populated field therefore
                    // rendered as an unlabelled row of text between two buttons
                    // and read as a static label -- the first outside tester
                    // could not find anywhere to change her name at all.
                    LabeledContent("Name") {
                        TextField("Your name", text: $displayName)
                            .multilineTextAlignment(.trailing)
                    }
                    DisclosureGroup("Verify my identity") {
                        Text(fingerprintWords(userId: identity.userId).joined(separator: " "))
                            .font(.body.monospaced())
                            .textSelection(.enabled)
                        Text("Have your friend match these words against your name in their contacts to confirm it's really you.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                Section("My friend card") {
                    Button {
                        showMyCard = true
                    } label: {
                        Label("Show my friend card", systemImage: "qrcode")
                    }
                }

            }
            .navigationTitle("Profile")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onAppear {
                displayName = appModel.displayName
                avatarImage = ProfilePhotoStore.loadAvatarImage()
            }
            .task(id: displayName) {
                try? await Task.sleep(nanoseconds: 350_000_000)
                guard !Task.isCancelled else { return }
                let trimmed = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
                guard trimmed != appModel.displayName else { return }
                ProfileStore.saveDisplayName(trimmed)
                appModel.displayName = trimmed
                syncProfile(epoch: ProfileStore.bumpOwnAvatarEpoch())
            }
            .onChange(of: photoItem) { item in
                guard let item else { return }
                Task {
                    guard let data = try? await item.loadTransferable(type: Data.self),
                          let image = UIImage(data: data),
                          let saved = ProfilePhotoStore.save(image: image) else { return }
                    await MainActor.run {
                        avatarImage = saved
                        syncProfile(epoch: ProfileStore.bumpOwnAvatarEpoch())
                    }
                }
            }
            .sheet(isPresented: $showMyCard) {
                MyQRView(identity: identity, displayName: displayName, onSayHi: { _ in })
            }
        }
    }

    private func syncProfile(epoch: Int64) {
        ProfileSyncSender.queueToAllContacts(
            store: AppStore.get(),
            identity: identity,
            displayName: displayName.trimmingCharacters(in: .whitespacesAndNewlines),
            epoch: epoch
        )
    }
}
