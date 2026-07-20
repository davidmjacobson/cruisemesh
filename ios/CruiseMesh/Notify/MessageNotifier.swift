import Foundation
import Combine
import UserNotifications

enum MessageNotifier {
    static let chatUserIdKey = "chatUserIdHex"
    static let chatIsGroupKey = "chatIsGroup"
    static let categoryId = "CRUISEMESH_MESSAGE"
    static let replyActionId = "CRUISEMESH_REPLY"
    static let markReadActionId = "CRUISEMESH_MARK_READ"

    static func requestPermission() {
        configureCategories()
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in }
    }

    static func configureCategories() {
        let reply = UNTextInputNotificationAction(
            identifier: replyActionId,
            title: "Reply",
            options: [],
            textInputButtonTitle: "Send",
            textInputPlaceholder: "Message"
        )
        let markRead = UNNotificationAction(identifier: markReadActionId, title: "Mark as read", options: [])
        let category = UNNotificationCategory(
            identifier: categoryId,
            actions: [reply, markRead],
            intentIdentifiers: [],
            options: []
        )
        UNUserNotificationCenter.current().setNotificationCategories([category])
    }

    static func notifyIncoming(contact: Contact, preview: String) {
        guard !ChatMuteStore.isMuted(contact.userId) else { return }
        let content = UNMutableNotificationContent()
        let name = coreContactDisplayName(contact: contact)
        content.title = name.isEmpty ? formatUserId(userId: contact.userId) : name
        content.body = preview
        content.sound = .default
        content.userInfo = [chatUserIdKey: UserIdHex.encode(contact.userId), chatIsGroupKey: false]
        content.categoryIdentifier = categoryId
        // Group in Notification Center by chat; a unique per-message request
        // id (not the chat id) so a burst of messages stacks instead of each
        // one replacing the last (FI11).
        content.threadIdentifier = UserIdHex.encode(contact.userId)

        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request, withCompletionHandler: nil)
    }

    static func notifyFriendAdded(contact: Contact) {
        let content = UNMutableNotificationContent()
        content.title = contact.name
        content.body = "\(contact.name) added you. Say hi."
        content.sound = .default
        content.userInfo = [chatUserIdKey: UserIdHex.encode(contact.userId), chatIsGroupKey: false]
        content.categoryIdentifier = categoryId
        // Own identifier prefix (FI11) so this can't be clobbered by, or
        // clobber, that contact's message notifications.
        let request = UNNotificationRequest(
            identifier: "friend-added:" + UserIdHex.encode(contact.userId),
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request, withCompletionHandler: nil)
    }

    static func notifyIncomingGroupMessage(group: Group, senderName: String, preview: String) {
        guard !ChatMuteStore.isMuted(group.id) else { return }
        let content = UNMutableNotificationContent()
        content.title = group.name
        content.body = "\(senderName): \(preview)"
        content.sound = .default
        content.userInfo = [chatUserIdKey: UserIdHex.encode(group.id), chatIsGroupKey: true]
        content.categoryIdentifier = categoryId
        // Group in Notification Center by chat; a unique per-message request
        // id (not the chat id) so a burst of messages stacks instead of each
        // one replacing the last (FI11).
        content.threadIdentifier = UserIdHex.encode(group.id)

        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request, withCompletionHandler: nil)
    }
}

enum NotificationOpenEvents {
    static let subject = PassthroughSubject<(Data, Bool), Never>()
}
