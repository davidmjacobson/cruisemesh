import SwiftUI
import UserNotifications

@main
struct CruiseMeshApp: App {
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
