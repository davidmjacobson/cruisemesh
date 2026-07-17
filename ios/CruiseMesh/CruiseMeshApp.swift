import SwiftUI
import UserNotifications

@main
struct CruiseMeshApp: App {
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
}
