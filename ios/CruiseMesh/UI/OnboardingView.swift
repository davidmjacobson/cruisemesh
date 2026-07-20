import PhotosUI
import SwiftUI
import UIKit

struct OnboardingView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    let onComplete: () -> Void

    @State private var page = 0
    @State private var displayName = ProfileStore.loadDisplayName()
    @State private var avatarImage = ProfilePhotoStore.loadAvatarImage()
    @State private var photoItem: PhotosPickerItem?
    @State private var showRestore = false

    private var defaultName: String {
        UIDevice.current.name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        VStack(spacing: 0) {
            TabView(selection: $page) {
                OnboardingSlide(
                    systemImage: "antenna.radiowaves.left.and.right",
                    title: "Messages that find a way through",
                    bodyText: "CruiseMesh delivers messages to people nearby even without Wi-Fi or cell service — using Bluetooth, local Wi-Fi, or hopping phone to phone.",
                    supportText: nil
                )
                .tag(0)

                OnboardingSlide(
                    systemImage: "point.3.connected.trianglepath.dotted",
                    title: "It uses whatever's around",
                    bodyText: "Nearby, messages travel phone-to-phone over Bluetooth and Wi-Fi. Farther away, they hop between other CruiseMesh phones until they reach your friend.",
                    supportText: "Always end-to-end encrypted — even the phones that help carry a message can't read it."
                )
                .tag(1)

                PermissionsSlide(
                    onEnable: {
                        MessageNotifier.requestPermission()
                        appModel.startMesh()
                    }
                )
                .tag(2)

                ProfileSetupSlide(
                    identity: identity,
                    displayName: $displayName,
                    avatarImage: $avatarImage,
                    photoItem: $photoItem,
                    defaultName: defaultName
                )
                .tag(3)
            }
            .tabViewStyle(.page(indexDisplayMode: .never))

            VStack(spacing: 14) {
                HStack(spacing: 8) {
                    ForEach(0..<4, id: \.self) { index in
                        Capsule()
                            .fill(index == page ? Color.accentColor : Color.secondary.opacity(0.28))
                            .frame(width: index == page ? 22 : 8, height: 8)
                    }
                }

                HStack {
                    if page > 0 {
                        Button("Back") {
                            withAnimation { page -= 1 }
                        }
                    }
                    Button("Restore from backup") {
                        showRestore = true
                    }
                    .buttonStyle(.borderless)
                    Spacer()
                    Button(page == 3 ? "Start using CruiseMesh" : "Next") {
                        if page == 3 {
                            complete()
                        } else {
                            withAnimation { page += 1 }
                        }
                    }
                    .buttonStyle(.borderedProminent)
                }
            }
            .padding(20)
            .background(.bar)
        }
        .onChange(of: photoItem) { item in
            guard let item else { return }
            Task {
                guard let data = try? await item.loadTransferable(type: Data.self),
                      let image = UIImage(data: data),
                      let saved = ProfilePhotoStore.save(image: image) else { return }
                await MainActor.run {
                    avatarImage = saved
                    ProfileStore.bumpOwnAvatarEpoch()
                }
            }
        }
        .sheet(isPresented: $showRestore) {
            BackupRestoreView {
                OnboardingStore.markCompleted()
            }
        }
    }

    private func complete() {
        let trimmed = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        let finalName = trimmed.isEmpty ? defaultName : trimmed
        ProfileStore.saveDisplayName(finalName)
        appModel.displayName = finalName
        if ProfileStore.loadOwnAvatarEpoch() == 0 {
            ProfileStore.bumpOwnAvatarEpoch()
        }
        OnboardingStore.markCompleted()
        appModel.startMesh()
        onComplete()
    }
}

private struct OnboardingSlide: View {
    let systemImage: String
    let title: String
    let bodyText: String
    let supportText: String?

    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: systemImage)
                .font(.system(size: 58, weight: .semibold))
                .foregroundStyle(Color.accentColor)
            Text(title)
                .font(.largeTitle.weight(.bold))
                .multilineTextAlignment(.center)
            Text(bodyText)
                .font(.title3)
                .multilineTextAlignment(.center)
            if let supportText {
                Text(supportText)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
        }
        .padding(28)
    }
}

private struct PermissionsSlide: View {
    let onEnable: () -> Void

    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: "checkmark.shield")
                .font(.system(size: 58, weight: .semibold))
                .foregroundStyle(Color.accentColor)
            Text("Turn on a few permissions")
                .font(.largeTitle.weight(.bold))
                .multilineTextAlignment(.center)
            Text("Each of these opens up another way for your messages to get through.")
                .font(.title3)
                .multilineTextAlignment(.center)
            Button("Enable Bluetooth and notifications", action: onEnable)
                .buttonStyle(.borderedProminent)
            Text("You can turn these on later in Settings.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(28)
    }
}

private struct ProfileSetupSlide: View {
    let identity: Identity
    @Binding var displayName: String
    @Binding var avatarImage: UIImage?
    @Binding var photoItem: PhotosPickerItem?
    let defaultName: String

    var body: some View {
        VStack(spacing: 18) {
            Text("What name would you like to go by?")
                .font(.largeTitle.weight(.bold))
                .multilineTextAlignment(.center)
            Text("This is what people will see when you share your friend card or add each other nearby. You can change it later.")
                .font(.body)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            AvatarView(
                userId: identity.userId,
                name: displayName.isEmpty ? defaultName : displayName,
                size: 92,
                photo: avatarImage
            )
            TextField("Display name", text: $displayName)
                .textFieldStyle(.roundedBorder)
                .onAppear {
                    if displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        displayName = defaultName
                    }
                }
            PhotosPicker(selection: $photoItem, matching: .images) {
                Label("Choose profile photo", systemImage: "photo")
            }
            Text(formatUserId(userId: identity.userId))
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        }
        .padding(28)
    }
}
