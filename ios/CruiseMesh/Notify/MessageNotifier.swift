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

    static func notifyFriendAdded(contact: Contact) {
        let content = UNMutableNotificationContent()
        content.title = contact.name
        content.body = "\(contact.name) added you. Say hi."
        content.sound = .default
        content.userInfo = [chatUserIdKey: UserIdHex.encode(contact.userId)]
        let request = UNNotificationRequest(
            identifier: UserIdHex.encode(contact.userId),
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request, withCompletionHandler: nil)
    }

    static func notifyIncomingGroupMessage(group: Group, senderName: String, preview: String) {
        let content = UNMutableNotificationContent()
        content.title = group.name
        content.body = "\(senderName): \(preview)"
        content.sound = .default
        content.userInfo = [chatUserIdKey: UserIdHex.encode(group.id)]

        let id = UserIdHex.encode(group.id)
        let request = UNNotificationRequest(identifier: id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request, withCompletionHandler: nil)
    }
}
