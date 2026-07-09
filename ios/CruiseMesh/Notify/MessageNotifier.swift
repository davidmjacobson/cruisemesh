import Foundation
import UserNotifications

enum MessageNotifier {
    static let chatUserIdKey = "chatUserIdHex"

    static func requestPermission() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in }
    }

    static func notifyIncoming(contact: Contact, preview: String) {
        let content = UNMutableNotificationContent()
        content.title = contact.name.isEmpty ? formatUserId(userId: contact.userId) : contact.name
        content.body = preview
        content.sound = .default
        content.userInfo = [chatUserIdKey: UserIdHex.encode(contact.userId)]

        let id = UserIdHex.encode(contact.userId)
        let request = UNNotificationRequest(identifier: id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request, withCompletionHandler: nil)
    }
}
