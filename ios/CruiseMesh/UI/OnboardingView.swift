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

    /// Slide count, and the index of the last one. Named rather than inlined
    /// because the page count previously appeared as a bare `4` in three
    /// places (dot row, button label, button action) and adding a slide meant
    /// finding all of them.
    private static let pageCount = 5
    private var lastPage: Int { Self.pageCount - 1 }

    private var defaultName: String {
        UIDevice.current.name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        VStack(spacing: 0) {
            TabView(selection: $page) {
                OnboardingSlide(
                    systemImage: "antenna.radiowaves.left.and.right",
                    title: "Messages that find a way through",
                    bodyText: "CruiseMesh gets your messages to friends and family using virtually every connection your phone has — Bluetooth, local Wi-Fi, and even handing them phone to phone when there's no signal at all.",
                    supportText: "Built for cruises, hikes, festivals, stadiums, road trips — anywhere the network is weak, missing, or overloaded."
                )
                .tag(0)

                // The internet clause is deliberately qualified: internet
                // delivery needs a Cruise Pass or a self-hosted relay, and
                // onboarding must not imply the free tier includes it.
                OnboardingSlide(
                    systemImage: "point.3.connected.trianglepath.dotted",
                    title: "It uses whatever's around",
                    bodyText: "Nearby, CruiseMesh talks phone-to-phone over Bluetooth and local Wi-Fi. Farther away, your message hops between other phones running CruiseMesh until it reaches your friend — and, with a Cruise Pass or your own server, over the internet whenever any of those phones has a connection.",
                    supportText: "Every message is encrypted end to end, so the phones and networks that help carry it can never read it."
                )
                .tag(1)

                PermissionsSlide(
                    onEnable: {
                        MessageNotifier.requestPermission()
                        appModel.startMesh()
                    }
                )
                .tag(2)

                // T5 slide 4: the least guessable thing about running
                // CruiseMesh -- staying on a Wi-Fi network with no internet is
                // useful, because the local network reaches nearby phones
                // faster than Bluetooth.
                OnboardingSlide(
                    systemImage: "wifi",
                    title: "Leave Wi-Fi on, even with no internet",
                    bodyText: "On a ship or anywhere the Wi-Fi has no internet, keep it connected anyway — CruiseMesh uses that local network to reach phones near you faster than Bluetooth alone.",
                    supportText: "CruiseMesh won't use the dead connection for the internet; it just uses it to find and talk to nearby phones."
                )
                .tag(3)

                ProfileSetupSlide(
                    identity: identity,
                    displayName: $displayName,
                    avatarImage: $avatarImage,
                    photoItem: $photoItem,
                    defaultName: defaultName
                )
                .tag(4)
            }
            .tabViewStyle(.page(indexDisplayMode: .never))

            VStack(spacing: 14) {
                HStack(spacing: 8) {
                    ForEach(0..<Self.pageCount, id: \.self) { index in
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
                    Button(page == lastPage ? "Start using CruiseMesh" : "Next") {
                        if page == lastPage {
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
            Text("Give CruiseMesh more ways to connect")
                .font(.largeTitle.weight(.bold))
                .multilineTextAlignment(.center)
            Text("Each of these opens up another path for your messages.")
                .font(.title3)
                .multilineTextAlignment(.center)
            Button("Enable Bluetooth and notifications", action: onEnable)
                .buttonStyle(.borderedProminent)
            Text("You can turn these on later in Settings — CruiseMesh just has fewer ways to reach people until you do.")
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
            Text("This is what people see when you share your friend card or add each other nearby. You can change it anytime.")
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
