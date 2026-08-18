import SwiftUI
import UIKit
import UserNotifications
import os.log

@main
struct CruiseMeshApp: App {
    // FI3: registers `AppDelegate` below so its
    // `application(_:didFinishLaunchingWithOptions:)` runs before SwiftUI
    // builds this struct's own view/state graph -- see that method's doc for
    // why a background BLE relaunch needs `MeshController` touched that
    // early.
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var appModel: AppModel
    /// `specs/multi-device-v1.md` §10 step 5. Watched from the root because the
    /// notice can land while somebody is looking at their chats, and read once at
    /// launch (see the `task` below) because a device ejected in a previous run
    /// must still know.
    @StateObject private var deviceRemoval = DeviceRemovalStatus.shared
    @State private var termsAccepted: Bool
    @State private var onboardingCompleted: Bool
    @AppStorage(AppearancePreference.storageKey, store: AppDefaults.current)
    private var appearanceRawValue = AppearancePreference.system.rawValue

    private var appearance: AppearancePreference {
        AppearancePreference(storedValue: appearanceRawValue)
    }

    private var appearanceBinding: Binding<AppearancePreference> {
        Binding(
            get: { appearance },
            set: { appearanceRawValue = $0.rawValue }
        )
    }

    init() {
        _appModel = StateObject(wrappedValue: AppModel())
        if let scenario = UITestConfiguration.scenario {
            _termsAccepted = State(initialValue: scenario.termsAccepted)
            _onboardingCompleted = State(initialValue: scenario.onboardingCompleted)
        } else {
            _termsAccepted = State(initialValue: TermsAcceptanceStore.isCurrentVersionAccepted())
            _onboardingCompleted = State(initialValue: OnboardingStore.isCompleted())
        }
    }

    var body: some Scene {
        WindowGroup {
            SwiftUI.Group {
                if !termsAccepted {
                    TermsAcceptanceView {
                        TermsAcceptanceStore.acceptCurrentVersion()
                        termsAccepted = true
                    }
                } else if deviceRemoval.removed {
                    // The core stage is the fact; this is the only screen that
                    // may be drawn on top of it, for the reason
                    // `DeviceRemovedView` states. Core already refuses to
                    // advertise, author or ack from that stage and
                    // `MeshController` stops itself when the notice lands -- but
                    // a mesh started before that, or restarted since by a
                    // background relaunch, has to be told to stay down rather
                    // than left running behind a screen that says the opposite.
                    DeviceRemovedView()
                        .onAppear { appModel.stopMesh() }
                } else if onboardingCompleted {
                    ChatListView(
                        identity: appModel.identity,
                        appModel: appModel,
                        appearance: appearanceBinding
                    )
                } else {
                    OnboardingView(identity: appModel.identity, appModel: appModel) {
                        onboardingCompleted = true
                    }
                }
            }
            .environmentObject(appModel)
            .defaultAppStorage(AppDefaults.current)
            .preferredColorScheme(appearance.colorScheme)
            .onAppear {
                guard !UITestConfiguration.isEnabled else { return }
                UNUserNotificationCenter.current().delegate = NotificationDelegate.shared
                MessageNotifier.configureCategories()
            }
            .task {
                // §10 step 5, at launch: a device its person ejected in an
                // earlier run holds the terminal stage in its store and must
                // find out again before it draws anything else. Off the main
                // thread for the same reason every other store read is, even
                // though this one is a single select against a one-row table.
                guard !UITestConfiguration.isEnabled else { return }
                await Task.detached(priority: .utility) {
                    DeviceRemovalStatus.shared.refresh(store: AppStore.get())
                }.value
            }
            .onOpenURL { url in
                // T20: the core owns the routing table so both shells agree,
                // and so the https link and the cruisemesh:// scheme resolve
                // identically. The scheme exists because iOS does not fire a
                // Universal Link for a same-domain navigation, which leaves
                // the website's "Open in CruiseMesh" button inert in Safari.
                guard
                    let route = deepLinkRoute(
                        scheme: url.scheme ?? "",
                        host: url.host ?? "",
                        path: url.path
                    ),
                    let fragment = url.fragment
                else { return }
                switch route {
                case .friend:
                    // Always hand the fragment to the friends screen. A future
                    // CMFRIEND4+/CMLINK scheme must fail soft there with
                    // "update the app", not vanish.
                    appModel.pendingFriendToken = fragment
                case .relaySetup:
                    guard (try? parseRelaySetupText(text: fragment)) != nil else { return }
                    appModel.pendingRelayCard = fragment
                case .lan:
                    guard let endpoint = parseLanEndpointLink(fragment) else { return }
                    LanTransportDiagnostics.shared.queueManualConnection(endpoint)
                    appModel.startMesh()
                case .deviceLink:
                    // A device-link offer is scanned inside the linking
                    // ceremony, by the device that is already part of this
                    // person. There is no screen to open cold yet, and opening
                    // the wrong one would drop someone into a flow that cannot
                    // finish what the link starts.
                    return
                }
            }
            .onChange(of: scenePhase) { phase in
                appModel.setAppForeground(phase == .active)
                guard !UITestConfiguration.isEnabled else { return }
                // Flush both when leaving and when returning: mesh work can
                // continue while backgrounded, and those entries should reach
                // the persistent tester archive before a later termination.
                DiagnosticLogExport.archiveCurrentSession()
            }
        }
    }
}

/// FI3: bridges `UIApplicationDelegate`'s launch-options callback into the
/// SwiftUI app lifecycle purely to catch a background BLE-triggered relaunch
/// as early as possible.
///
/// On an ordinary user-initiated launch (tapping the app icon, or a system
/// launch for any other reason) `launchOptions` never carries the Bluetooth
/// keys checked below, so this does nothing and `AppModel`'s own
/// onboarding-gated startup (`ChatListView`'s `onAppear` ->
/// `appModel.startMeshIfEnabled()`) is unaffected -- this is not a
/// replacement for that path, only a fast-path for the one scenario it
/// can't cover.
///
/// A BLE-triggered relaunch, though, may never show any UI at all (the
/// process can be woken, do its background work, and get suspended again
/// without `ChatListView` ever appearing), so waiting for that path is not
/// safe. `BleTransport`'s `CBCentralManager`/`CBPeripheralManager` were
/// created with restoration identifiers (FI3) specifically so the system
/// can redeliver `willRestoreState` to them -- but that only helps once
/// `MeshController.shared` (and therefore `BleTransport`) actually exists,
/// and `MeshController.start()` has run far enough to wire its frame/
/// connection callbacks (see `MeshController.start()`'s callback
/// assignments) -- otherwise a restored peripheral's frames arrive at
/// `BleTransport.onFrame`, which is still `nil`, and are silently dropped.
/// Both only happen today when something calls into `MeshController`, which
/// this provides for the background-relaunch case specifically.
final class AppDelegate: NSObject, UIApplicationDelegate {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "AppDelegate")

    func applicationWillTerminate(_ application: UIApplication) {
        guard !UITestConfiguration.isEnabled else { return }
        DiagnosticLogExport.archiveCurrentSession()
    }

    @MainActor
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        if UITestConfiguration.isEnabled {
            UIView.setAnimationsEnabled(false)
            return true
        }
        // Battery audit, 2026-07-21: registered unconditionally, here rather
        // than in the SwiftUI `onAppear` below, for the same reason this
        // method exists at all -- a background BLE relaunch may never show
        // any UI, so MetricKit's "call this as early as possible in every
        // launch" guidance needs a hook that always runs. See
        // MetricKitCollector's doc.
        MetricKitCollector.shared.start()
        UNUserNotificationCenter.current().delegate = NotificationDelegate.shared
        MessageNotifier.configureCategories()
        MessageNotifier.registerForRemoteNotificationsIfAuthorized()
        // Same reasoning: Background App Refresh has to be sampled on the main
        // actor, and a headless relaunch is exactly the case where knowing it
        // is denied matters most.
        EnvironmentSnapshot.start()
        // Which Shore Pass this device is on, before anything tries to use it.
        RelayConfigStore.logSummary()
        let isBluetoothRelaunch = launchOptions?[.bluetoothCentrals] != nil
            || launchOptions?[.bluetoothPeripherals] != nil
        // Onboarding gates mesh startup deliberately (permissions are
        // requested as part of that flow, not before it) -- a fresh install
        // can never be a Bluetooth relaunch anyway (nothing was ever
        // scanning/advertising/connected to restore), but guard explicitly
        // rather than relying on that.
        guard isBluetoothRelaunch,
              TermsAcceptanceStore.isCurrentVersionAccepted(),
              OnboardingStore.isCompleted() else { return true }
        let identity = IdentityStore.loadOrCreate()
        MeshController.shared.configure(identity: identity)
        let meshEnabled = AppDefaults.current.object(forKey: AppModel.meshEnabledKey) == nil
            || AppDefaults.current.bool(forKey: AppModel.meshEnabledKey)
        if meshEnabled {
            MeshController.shared.start()
        }
        return true
    }

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        guard !UITestConfiguration.isEnabled else { return }
        RemoteNotificationTokenStore.save(deviceToken)
        RemotePushRegistrationClient.syncCurrentIfPossible()
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        guard !UITestConfiguration.isEnabled else { return }
        Self.log.warning("APNs registration failed: \(error.localizedDescription, privacy: .public)")
    }

    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        guard userInfo["cruisemesh_relay_wake"] as? Bool == true,
              TermsAcceptanceStore.isCurrentVersionAccepted(),
              OnboardingStore.isCompleted(),
              RelayConfigStore.load() != nil
        else {
            completionHandler(.noData)
            return
        }
        let meshEnabled = AppDefaults.current.object(forKey: AppModel.meshEnabledKey) == nil
            || AppDefaults.current.bool(forKey: AppModel.meshEnabledKey)
        guard meshEnabled else {
            completionHandler(.noData)
            return
        }

        let identity = IdentityStore.loadOrCreate()
        MeshController.shared.configure(identity: identity)
        MeshController.shared.start()
        MeshController.shared.handleRemoteRelayWake { completed in
            completionHandler(completed ? .newData : .failed)
        }
    }
}

final class NotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
    static let shared = NotificationDelegate()

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        defer { completionHandler() }
        guard TermsAcceptanceStore.isCurrentVersionAccepted() else { return }
        let info = response.notification.request.content.userInfo
        guard let hex = info[MessageNotifier.chatUserIdKey] as? String,
              let chatId = try? UserIdHex.decode(hex) else { return }
        defer { MessageNotifier.clearChatNotifications(chatId: chatId) }
        let isGroup = info[MessageNotifier.chatIsGroupKey] as? Bool ?? false
        let store = AppStore.get()
        let identity = IdentityStore.loadOrCreate()

        if response.actionIdentifier == MessageNotifier.replyActionId,
           let textResponse = response as? UNTextInputNotificationResponse {
            let text = textResponse.userText.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { return }
            if isGroup, let group = try? store.getGroup(groupId: chatId) {
                GroupSender(store: store, identity: identity).sendText(group: group, text: text)
            } else if let contact = try? store.getContact(userId: chatId) {
                RealMeshSender(store: store, identity: identity).sendText(contact: contact, text: text)
            }
        } else if response.actionIdentifier == MessageNotifier.markReadActionId {
            ChatReadMarker.markRead(
                store: store,
                ownUserId: identity.userId,
                chatId: chatId,
                isGroup: isGroup
            )
            ChatEvents.notifyChatChanged(chatId)
        } else if response.actionIdentifier == UNNotificationDefaultActionIdentifier {
            DispatchQueue.main.async { NotificationOpenEvents.subject.send((chatId, isGroup)) }
        }
    }
}
