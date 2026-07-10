import SwiftUI
import UserNotifications

@main
struct CruiseMeshApp: App {
    @StateObject private var appModel = AppModel()

    var body: some Scene {
        WindowGroup {
            ChatListView(identity: appModel.identity, appModel: appModel)
                .environmentObject(appModel)
                .onAppear {
                    UNUserNotificationCenter.current().delegate = NotificationDelegate.shared
                }
                .onOpenURL { url in
                    // Reserved for future cruisemesh:// deep links
                    _ = url
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
