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
                    title: "Welcome to CruiseMesh",
                    bodyText: "CruiseMesh helps you communicate with friends and family nearby, even when you do not have Wi-Fi or cell service.",
                    supportText: "Built for the moments when networks disappear. Keep conversations moving on hikes, cruises, festivals, road trips, and anywhere coverage is unreliable."
                )
                .tag(0)

                OnboardingSlide(
                    systemImage: "point.3.connected.trianglepath.dotted",
                    title: "Messages can hop phone to phone",
                    bodyText: "CruiseMesh uses your phone and other phones running CruiseMesh to help deliver messages, even when you are too far from your friend to connect directly over Bluetooth.",
                    supportText: "Your messages are encrypted end to end, so nearby relays can help carry them without being able to read them."
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
    let supportText: String

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
            Text(supportText)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
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
            Text("Turn on the permissions that help the mesh")
                .font(.largeTitle.weight(.bold))
                .multilineTextAlignment(.center)
            Text("CruiseMesh works best when iOS lets it use Bluetooth and show delivery notifications.")
                .font(.title3)
                .multilineTextAlignment(.center)
            Button("Enable Bluetooth and notifications", action: onEnable)
                .buttonStyle(.borderedProminent)
            Text("You can continue without these, but delivery is less reliable while the app is backgrounded.")
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
        }
        .padding(28)
    }
}
