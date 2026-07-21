import SwiftUI
import UIKit
import UserNotifications

@main
struct CruiseMeshApp: App {
    // FI3: registers `AppDelegate` below so its
    // `application(_:didFinishLaunchingWithOptions:)` runs before SwiftUI
    // builds this struct's own view/state graph -- see that method's doc for
    // why a background BLE relaunch needs `MeshController` touched that
    // early.
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var appModel = AppModel()
    @State private var onboardingCompleted = OnboardingStore.isCompleted()

    var body: some Scene {
        WindowGroup {
            SwiftUI.Group {
                if onboardingCompleted {
                    ChatListView(identity: appModel.identity, appModel: appModel)
                } else {
                    OnboardingView(identity: appModel.identity, appModel: appModel) {
                        onboardingCompleted = true
                    }
                }
            }
            .environmentObject(appModel)
            .onAppear {
                UNUserNotificationCenter.current().delegate = NotificationDelegate.shared
                MessageNotifier.configureCategories()
            }
            .onOpenURL { url in
                guard url.host == "cruisemesh.app", let fragment = url.fragment else { return }
                if url.path == "/f" || url.path == "/f/" {
                    guard (try? parseFriendText(text: fragment)) != nil else { return }
                    appModel.pendingFriendToken = fragment
                } else if url.path == "/lan" || url.path == "/lan/" {
                    guard let endpoint = parseLanEndpointLink(fragment) else { return }
                    LanTransportDiagnostics.shared.queueManualConnection(endpoint)
                    appModel.startMesh()
                }
            }
            .onChange(of: scenePhase) { phase in
                appModel.setAppForeground(phase == .active)
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
    @MainActor
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        let isBluetoothRelaunch = launchOptions?[.bluetoothCentrals] != nil
            || launchOptions?[.bluetoothPeripherals] != nil
        // Onboarding gates mesh startup deliberately (permissions are
        // requested as part of that flow, not before it) -- a fresh install
        // can never be a Bluetooth relaunch anyway (nothing was ever
        // scanning/advertising/connected to restore), but guard explicitly
        // rather than relying on that.
        guard isBluetoothRelaunch, OnboardingStore.isCompleted() else { return true }
        let identity = IdentityStore.loadOrCreate()
        MeshController.shared.configure(identity: identity)
        let meshEnabled = UserDefaults.standard.object(forKey: AppModel.meshEnabledKey) == nil
            || UserDefaults.standard.bool(forKey: AppModel.meshEnabledKey)
        if meshEnabled {
            MeshController.shared.start()
        }
        return true
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
        let info = response.notification.request.content.userInfo
        guard let hex = info[MessageNotifier.chatUserIdKey] as? String,
              let chatId = try? UserIdHex.decode(hex) else { return }
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
            let senderIds: [Data]
            if isGroup, let group = try? store.getGroup(groupId: chatId) {
                senderIds = group.memberUserIds.filter { $0 != identity.userId }
            } else {
                senderIds = [chatId]
            }
            for senderId in senderIds {
                let through = (try? store.highestLamport(chatId: chatId, senderUserId: senderId)) ?? 0
                if through > 0 {
                    try? store.recordOutgoingReceipt(
                        chatId: chatId,
                        senderUserId: senderId,
                        receiptType: ReceiptType.read,
                        throughLamport: through
                    )
                }
            }
            ChatEvents.notifyChatChanged(chatId)
        } else if response.actionIdentifier == UNNotificationDefaultActionIdentifier {
            DispatchQueue.main.async { NotificationOpenEvents.subject.send((chatId, isGroup)) }
        }
    }
}
