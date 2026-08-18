import CoreBluetooth
import PhotosUI
import SwiftUI
import UIKit
import UserNotifications

struct OnboardingView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    let onComplete: () -> Void

    @State private var page = 0
    @State private var displayName = ProfileStore.loadStoredDisplayName()
    @State private var avatarImage = ProfilePhotoStore.loadAvatarImage()
    @State private var photoItem: PhotosPickerItem?
    @State private var showRestore = false
    @State private var showAddDevice = false
    /// One of §9's two doors adopted this phone onto a person's existing fleet.
    /// Acted on when the sheet that carried the ceremony has actually gone —
    /// see `enterTheAppIfThisPhoneWasAdopted`.
    @State private var adoptedByAnotherDevice = false

    /// Slide count, and the index of the last one. Named rather than inlined
    /// because the page count previously appeared as a bare `4` in three
    /// places (dot row, button label, button action) and adding a slide meant
    /// finding all of them.
    private static let pageCount = 6
    private var lastPage: Int { Self.pageCount - 1 }

    // There is deliberately no device-name default here. `UIDevice.current.name`
    // stopped returning the user's chosen device name in iOS 16 unless the app
    // holds Apple's entitlement for it, so every install gets the bare model
    // string instead. Pre-filling the profile slide with it made the question
    // look already answered: testers tapped past and shipped as "iPhone",
    // indistinguishable from one another in every contact list. The name is now
    // required and nothing substitutes one silently.
    private var trimmedName: String {
        displayName.trimmingCharacters(in: .whitespacesAndNewlines)
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
                // delivery needs a Shore Pass or a self-hosted relay, and
                // onboarding must not imply the free tier includes it.
                OnboardingSlide(
                    systemImage: "point.3.connected.trianglepath.dotted",
                    title: "It uses whatever's around",
                    bodyText: "Nearby, CruiseMesh talks phone-to-phone over Bluetooth and local Wi-Fi. Farther away, your message hops between other phones running CruiseMesh until it reaches your friend — and, with a Shore Pass or your own server, over the internet whenever any of those phones has a connection.",
                    supportText: "Every message is encrypted end to end, so the phones and networks that help carry it can never read it."
                )
                .tag(1)

                // The one paid part, introduced where expectations are being
                // set anyway: what a Shore Pass is for, that everything nearby
                // stays free, and that one pass covers the family.
                // Deliberately informational -- no purchase link here; the
                // Shore Pass screen in Settings is where the doing happens.
                OnboardingSlide(
                    systemImage: "ticket",
                    title: "Shore Pass carries messages over the internet",
                    bodyText: "Everything nearby — Bluetooth, local Wi-Fi, groups, encryption — is free, always. A Shore Pass is the one optional extra: it delivers your family's messages over the internet when you're spread out or someone's back home.",
                    supportText: "One pass covers your whole family. Somebody buys it once at cruisemesh.app, then shares the setup card with everyone. If your family already has one, all you need is their card."
                )
                .tag(2)

                PermissionsSlide(
                    onEnable: {
                        MessageNotifier.requestPermission()
                        appModel.startMesh()
                    }
                )
                .tag(3)

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
                .tag(4)

                ProfileSetupSlide(
                    identity: identity,
                    displayName: $displayName,
                    avatarImage: $avatarImage,
                    photoItem: $photoItem
                )
                .tag(5)
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
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("Page \(page + 1) of \(Self.pageCount)")

                ViewThatFits(in: .horizontal) {
                    HStack {
                        backButton
                        restoreButton
                        anotherDeviceButton
                        Spacer()
                        primaryButton
                    }
                    VStack(spacing: 10) {
                        primaryButton
                            .frame(maxWidth: .infinity, alignment: .trailing)
                        HStack {
                            backButton
                            Spacer()
                            restoreButton
                        }
                        HStack {
                            Spacer()
                            anotherDeviceButton
                        }
                    }
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
        .sheet(isPresented: $showRestore, onDismiss: enterTheAppIfThisPhoneWasAdopted) {
            BackupRestoreView(
                onStaged: { OnboardingStore.markCompleted() },
                onAdopted: { adoptedByAnotherDevice = true }
            )
        }
        .sheet(isPresented: $showAddDevice, onDismiss: {
            // Tapping Done is not the only way out of a finished ceremony: a
            // sheet can be swiped away. `LinkAdoption` made this phone set up
            // durably at the moment the adoption completed, and nothing on this
            // door writes that flag for any other reason, so it is the same
            // evidence one tap later. (The restore door is not asked the same
            // question: a staged restore marks setup complete too, and that one
            // wants a relaunch rather than the app.)
            if OnboardingStore.isCompleted() { adoptedByAnotherDevice = true }
            enterTheAppIfThisPhoneWasAdopted()
        }) {
            NavigationStack {
                AddDeviceView(
                    identity: identity,
                    role: .newDevice,
                    expectedPersonId: nil,
                    onLinked: { adoptedByAnotherDevice = true }
                )
            }
        }
        .accessibilityIdentifier("screen.onboarding")
    }

    @ViewBuilder private var backButton: some View {
        if page > 0 {
            Button("Back") {
                withAnimation { page -= 1 }
            }
        }
    }

    private var restoreButton: some View {
        Button("Restore from backup") {
            showRestore = true
        }
        .buttonStyle(.borderless)
    }

    /// The second door out of a new install (`specs/multi-device-v1.md` §9).
    ///
    /// There are two because there are two people here, not one. "Restore from
    /// backup" is for someone who saved a file and is coming back. This is for
    /// someone who bought a second phone and never made a backup at all — the
    /// ordinary case, and one that had no door before: without it, the only way
    /// onto a person's fleet was through a file they were never told to make.
    ///
    /// No person id to expect: they have not opened a backup, so the only thing
    /// this phone knows is that the other one is theirs, and the ceremony's own
    /// confirm is what checks that.
    private var anotherDeviceButton: some View {
        Button("This is another of my devices") {
            showAddDevice = true
        }
        .buttonStyle(.borderless)
    }

    private var primaryButton: some View {
        Button(page == lastPage ? "Start using CruiseMesh" : "Next") {
            if page == lastPage {
                complete()
            } else {
                withAnimation { page += 1 }
            }
        }
        .buttonStyle(.borderedProminent)
        .disabled(page == lastPage && trimmedName.isEmpty)
    }

    /// §9's other ending, and §9's closing paragraph: the two doors out of
    /// first-run setup land a person in the same place.
    ///
    /// A phone that was adopted holds this person's contacts, groups and history,
    /// so this wizard has nothing left to ask it — and asking anyway is what the
    /// two-phone session on 2026-08-18 saw: back on slide one, still offering the
    /// door it had just come through, and then asking a linked person their own
    /// name, which any answer to would have been a second name for one person.
    ///
    /// Run from the sheets' `onDismiss` rather than from the callback itself, so
    /// the ceremony's sheet is already gone before this screen is replaced.
    private func enterTheAppIfThisPhoneWasAdopted() {
        // Whatever happened behind the sheet, the profile may have changed under
        // this screen: an adoption writes the person's own name and photo
        // (`LinkAdoption`), and a restore writes both as well. Re-read before
        // anything else, because the one thing the name slide must never do is
        // show an empty field to somebody who already has a name.
        displayName = ProfileStore.loadStoredDisplayName()
        avatarImage = ProfilePhotoStore.loadAvatarImage()
        guard adoptedByAnotherDevice else { return }
        adoptedByAnotherDevice = false
        appModel.displayName = ProfileStore.loadDisplayName()
        appModel.startMesh()
        onComplete()
    }

    private func complete() {
        // Mirrors the button's own guard: the last slide cannot be completed
        // without a name, so there is no fallback to fall back to.
        let finalName = trimmedName
        guard !finalName.isEmpty else { return }
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
        ScrollView {
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
            .frame(maxWidth: .infinity)
            .padding(28)
        }
    }
}

/// What the permissions slide can usefully offer, given the decisions iOS has
/// already recorded.
///
/// Asking again after the system has recorded an answer is a silent no-op: no
/// prompt, no state change, nothing. So once every answer is in, the primary
/// button must stop being a re-request — either there is nothing left to do, or
/// the only place that can change the answer is Settings.
enum OnboardingPermissionAction: Equatable {
    /// At least one decision is still open, so asking can still show a prompt.
    case request
    /// Every decision is in and at least one was "no".
    case openSettings
    /// Every decision is in and all of them were "yes".
    case allSet
}

/// Pure mapping from the two system authorization states to what the slide
/// should show. Kept free of any view or framework state so it can be tested.
enum OnboardingPermissions {
    static func isBluetoothUndecided(_ bluetooth: CBManagerAuthorization) -> Bool {
        bluetooth == .notDetermined
    }

    static func isBluetoothBlocked(_ bluetooth: CBManagerAuthorization) -> Bool {
        bluetooth == .denied || bluetooth == .restricted
    }

    static func areNotificationsUndecided(_ notifications: UNAuthorizationStatus) -> Bool {
        notifications == .notDetermined
    }

    /// Only an explicit denial counts as blocked: provisional and ephemeral
    /// authorizations still deliver, and an unrecognised future case must not
    /// send anyone to Settings for a permission that may well be granted.
    static func areNotificationsBlocked(_ notifications: UNAuthorizationStatus) -> Bool {
        notifications == .denied
    }

    static func action(
        bluetooth: CBManagerAuthorization,
        notifications: UNAuthorizationStatus
    ) -> OnboardingPermissionAction {
        // Undecided wins over denied in the mixed case: the prompt for the open
        // one is still worth showing, and once it is answered this returns
        // `.openSettings` for whatever was refused.
        if isBluetoothUndecided(bluetooth) || areNotificationsUndecided(notifications) {
            return .request
        }
        if isBluetoothBlocked(bluetooth) || areNotificationsBlocked(notifications) {
            return .openSettings
        }
        return .allSet
    }
}

private struct PermissionsSlide: View {
    let onEnable: () -> Void

    @Environment(\.scenePhase) private var scenePhase
    // Read statically rather than through `BluetoothAccess.shared`: this slide
    // only needs the recorded decision, and touching that singleton would spin
    // up a `CBCentralManager` before the user has agreed to anything.
    @State private var bluetooth: CBManagerAuthorization = UITestConfiguration.isEnabled
        ? .allowedAlways : CBCentralManager.authorization
    @State private var notifications: UNAuthorizationStatus = UITestConfiguration.isEnabled
        ? .authorized : .notDetermined

    private var action: OnboardingPermissionAction {
        OnboardingPermissions.action(bluetooth: bluetooth, notifications: notifications)
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 20) {
                Image(systemName: action == .allSet ? "checkmark.circle.fill" : "checkmark.shield")
                    .font(.system(size: 58, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                Text("Give CruiseMesh more ways to connect")
                    .font(.largeTitle.weight(.bold))
                    .multilineTextAlignment(.center)
                Text("Each of these opens up another path for your messages.")
                    .font(.title3)
                    .multilineTextAlignment(.center)
                callToAction
                footnote
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                // Radios are the subject of this slide, and airplane mode is the
                // one radio setting that decides whether the trip ends with a
                // roaming bill. Same supporting style as the footnote above it.
                Text("On board, turn on airplane mode, then turn Wi-Fi and Bluetooth back on. CruiseMesh works fully without cellular, and airplane mode prevents roaming charges.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity)
            .padding(28)
        }
        .onAppear(perform: refreshPermissions)
        .onChange(of: scenePhase) { phase in
            // The permission alerts, and a trip to Settings, both take the
            // scene out of `.active` and bring it back. Re-deriving here is what
            // makes the control reflect the answer without leaving the slide.
            guard phase == .active else { return }
            refreshPermissions()
        }
    }

    @ViewBuilder private var callToAction: some View {
        switch action {
        case .request:
            Button("Enable Bluetooth and notifications") {
                onEnable()
                refreshPermissions()
            }
            .buttonStyle(.borderedProminent)
        case .openSettings:
            Button("Open Settings") {
                openSettings()
            }
            .buttonStyle(.borderedProminent)
        case .allSet:
            // Deliberately not a button: pressing one here would do nothing.
            Label("Bluetooth and notifications are on", systemImage: "checkmark.circle.fill")
                .font(.title3.weight(.semibold))
                .foregroundStyle(.green)
        }
    }

    @ViewBuilder private var footnote: some View {
        switch action {
        case .request:
            Text("You can turn these on later in Settings — CruiseMesh just has fewer ways to reach people until you do.")
        case .openSettings:
            if OnboardingPermissions.isBluetoothBlocked(bluetooth)
                && OnboardingPermissions.areNotificationsBlocked(notifications) {
                Text("Bluetooth and notifications are turned off for CruiseMesh. Turn them on in Settings to add those ways of reaching people.")
            } else if OnboardingPermissions.isBluetoothBlocked(bluetooth) {
                Text("Bluetooth is turned off for CruiseMesh. Turn it on in Settings to reach people nearby without any network.")
            } else {
                Text("Notifications are turned off for CruiseMesh. Turn them on in Settings to hear about messages as they arrive.")
            }
        case .allSet:
            Text("You can change these anytime in Settings.")
        }
    }

    private func refreshPermissions() {
        guard !UITestConfiguration.isEnabled else {
            bluetooth = .allowedAlways
            notifications = .authorized
            return
        }
        let bluetoothNow = CBCentralManager.authorization
        UNUserNotificationCenter.current().getNotificationSettings { settings in
            let status = settings.authorizationStatus
            Task { @MainActor in
                bluetooth = bluetoothNow
                notifications = status
            }
        }
    }

    /// Same destination as `BluetoothAccess.openSystemSettings()`, kept as its
    /// own function so this slide does not have to construct that singleton.
    private func openSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }
}

private struct ProfileSetupSlide: View {
    let identity: Identity
    @Binding var displayName: String
    @Binding var avatarImage: UIImage?
    @Binding var photoItem: PhotosPickerItem?

    private var isNameEmpty: Bool {
        displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 18) {
                Text("What name would you like to go by?")
                    .font(.largeTitle.weight(.bold))
                    .multilineTextAlignment(.center)
                Text("This is what people see when you share your friend card or add each other nearby. You can change it anytime.")
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                // With no name yet the avatar draws a neutral person glyph. Its own
                // accessibility label would fall back to the formatted user id, so
                // override it here: nowhere in onboarding should a person be read
                // their own identifier.
                AvatarView(
                    userId: identity.userId,
                    name: displayName,
                    size: 92,
                    photo: avatarImage
                )
                .accessibilityLabel("Your profile picture")
                TextField("Your name", text: $displayName)
                    .textFieldStyle(.roundedBorder)
                if isNameEmpty {
                    Text("Enter a name to continue.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                PhotosPicker(selection: $photoItem, matching: .images) {
                    Label("Choose profile photo", systemImage: "photo")
                }
            }
            .frame(maxWidth: .infinity)
            .padding(28)
        }
    }
}
