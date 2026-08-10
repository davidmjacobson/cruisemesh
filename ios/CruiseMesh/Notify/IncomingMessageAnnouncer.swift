import Foundation

protocol IncomingMessageAnnouncing {
    func announceDirectMessage(contact: Contact, preview: String)
    func announceGroupMessage(group: Group, senderName: String, preview: String)
    func announceGroupInvite(group: Group)
    func announceFriendAdded(contact: Contact)
    func announceSharedRequest(name: String, userId: Data)
}

struct LocalNotificationAnnouncer: IncomingMessageAnnouncing {
    func announceDirectMessage(contact: Contact, preview: String) {
        MessageNotifier.notifyIncoming(contact: contact, preview: preview)
    }

    func announceGroupMessage(group: Group, senderName: String, preview: String) {
        MessageNotifier.notifyIncomingGroupMessage(
            group: group,
            senderName: senderName,
            preview: preview
        )
    }

    func announceGroupInvite(group: Group) {
        MessageNotifier.notifyGroupInvite(group: group)
    }

    func announceFriendAdded(contact: Contact) {
        MessageNotifier.notifyFriendAdded(contact: contact)
    }

    func announceSharedRequest(name: String, userId: Data) {
        MessageNotifier.notifySharedRequest(name: name, userId: userId)
    }
}

/// The receive path's single notification decision point. All BLE, LAN, and
/// relay arrivals converge before this gate, so a newly stored visible chat
/// kind announces once when its chat is off screen and never when it is open.
final class IncomingMessageAnnouncementGate {
    private let announcer: IncomingMessageAnnouncing

    init(announcer: IncomingMessageAnnouncing) {
        self.announcer = announcer
    }

    func announceDirectIfNeeded(
        chatVisible: Bool,
        kind: UInt8,
        contact: Contact,
        preview: () -> String
    ) {
        guard !chatVisible, isVisibleChatKind(kind) else { return }
        announcer.announceDirectMessage(contact: contact, preview: preview())
    }

    func announceGroupIfNeeded(
        chatVisible: Bool,
        kind: UInt8,
        group: Group,
        senderName: () -> String,
        preview: () -> String
    ) {
        guard !chatVisible, isVisibleChatKind(kind) else { return }
        announcer.announceGroupMessage(
            group: group,
            senderName: senderName(),
            preview: preview()
        )
    }

    func announceGroupInviteIfNeeded(chatVisible: Bool, group: Group) {
        guard !chatVisible else { return }
        announcer.announceGroupInvite(group: group)
    }

    func announceFriendAdded(contact: Contact) {
        announcer.announceFriendAdded(contact: contact)
    }

    func announceSharedRequest(name: String, userId: Data) {
        announcer.announceSharedRequest(name: name, userId: userId)
    }
}
