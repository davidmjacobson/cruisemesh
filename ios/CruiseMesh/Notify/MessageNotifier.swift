import Foundation
import Combine
import UIKit
import UserNotifications

enum MessageNotifier {
    static let chatUserIdKey = "chatUserIdHex"
    static let chatIsGroupKey = "chatIsGroup"
    static let categoryId = "CRUISEMESH_MESSAGE"
    static let replyActionId = "CRUISEMESH_REPLY"
    static let markReadActionId = "CRUISEMESH_MARK_READ"

    static func requestPermission() {
        configureCategories()
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
            guard granted else { return }
            DispatchQueue.main.async {
                UIApplication.shared.registerForRemoteNotifications()
            }
        }
    }

    static func registerForRemoteNotificationsIfAuthorized() {
        UNUserNotificationCenter.current().getNotificationSettings { settings in
            guard settings.authorizationStatus == .authorized
                    || settings.authorizationStatus == .provisional
            else { return }
            DispatchQueue.main.async {
                UIApplication.shared.registerForRemoteNotifications()
            }
        }
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

    /// Somebody a friend passed our card to is asking to connect. Nothing has
    /// been imported yet -- tapping through lands on the confirmation, which is
    /// the whole point of a shared card (specs/share-contact.md decision 5).
    static func notifySharedRequest(name: String, userId: Data) {
        let content = UNMutableNotificationContent()
        content.title = name
        content.body = "\(name) wants to connect"
        content.sound = .default
        // Deliberately no chat userInfo and no reply/mark-read category: there
        // is no chat to open, and offering Reply on a request nobody accepted
        // would be a lie. Tapping it just opens the app, where the request is
        // waiting under Friends.
        // Own identifier prefix (FI11), and a fixed one per requester: a
        // redelivery must replace the waiting prompt, never stack another.
        let request = UNNotificationRequest(
            identifier: "shared-request:" + UserIdHex.encode(userId),
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

    static func notifyGroupInvite(group: Group) {
        let content = UNMutableNotificationContent()
        content.title = group.name
        content.body = String(localized: "Added you to \(group.name)")
        content.sound = .default
        content.userInfo = [chatUserIdKey: UserIdHex.encode(group.id), chatIsGroupKey: true]
        content.categoryIdentifier = categoryId
        content.threadIdentifier = UserIdHex.encode(group.id)

        let request = UNNotificationRequest(
            identifier: "group-invite:" + UserIdHex.encode(group.id),
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request, withCompletionHandler: nil)
    }

    /// Removes every delivered or not-yet-delivered request belonging to one
    /// chat. iOS uses unique per-message identifiers so bursts can stack, so
    /// cancellation must filter by the collision-free routing key in
    /// `userInfo` rather than assuming one request id per chat.
    static func clearChatNotifications(chatId: Data) {
        let center = UNUserNotificationCenter.current()
        let chatHex = UserIdHex.encode(chatId)
        center.getDeliveredNotifications { notifications in
            let ids = notifications.compactMap { notification in
                notification.request.content.userInfo[chatUserIdKey] as? String == chatHex
                    ? notification.request.identifier
                    : nil
            }
            if !ids.isEmpty { center.removeDeliveredNotifications(withIdentifiers: ids) }
        }
        center.getPendingNotificationRequests { requests in
            let ids = requests.compactMap { request in
                request.content.userInfo[chatUserIdKey] as? String == chatHex
                    ? request.identifier
                    : nil
            }
            if !ids.isEmpty { center.removePendingNotificationRequests(withIdentifiers: ids) }
        }
    }
}

enum NotificationOpenEvents {
    static let subject = PassthroughSubject<(Data, Bool), Never>()
}
