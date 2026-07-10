import Combine
import SwiftUI

struct GroupChatView: View {
    let group: Group
    let identity: Identity

    @Environment(\.dismiss) private var dismiss
    @State private var messages: [StoredMessage] = []
    @State private var draft = ""
    @State private var showDetails = false
    @State private var confirmDelete = false
    @State private var cancellable: AnyCancellable?

    private let store = AppStore.get()
    private var sender: GroupSender { GroupSender(store: store, identity: identity) }
    private var visibleMessages: [StoredMessage] {
        messages.filter { $0.kind == ProtocolKind.text || $0.kind == ProtocolKind.groupInvite }
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(visibleMessages.enumerated()), id: \.offset) { index, message in
                            if isNewDay(index) {
                                Text(dayLabel(message.timestamp))
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                                    .frame(maxWidth: .infinity)
                                    .padding(.vertical, 6)
                            }
                            GroupMessageRow(
                                message: message,
                                isOwn: message.senderUserId == identity.userId,
                                senderName: senderName(message.senderUserId),
                                groupName: group.name
                            )
                            .id(messageId(message))
                        }
                    }
                    .padding(.horizontal, 12)
                }
                .onChange(of: visibleMessages.count) { _ in
                    if let last = visibleMessages.last {
                        withAnimation { proxy.scrollTo(messageId(last), anchor: .bottom) }
                    }
                }
            }

            HStack(spacing: 8) {
                TextField("Message", text: $draft, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...4)
                Button("Send") {
                    let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else { return }
                    sender.sendText(group: group, text: text)
                    draft = ""
                    reload()
                }
                .buttonStyle(.borderedProminent)
            }
            .padding(12)
        }
        .navigationTitle(group.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Button { showDetails = true } label: {
                    HStack {
                        AvatarView(userId: group.id, name: group.name, size: 32)
                        VStack(alignment: .leading) {
                            Text(group.name).font(.headline)
                            Text("\(group.memberUserIds.count) members")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .buttonStyle(.plain)
            }
        }
        .onAppear {
            ChatVisibility.setVisible(group.id)
            MeshController.shared.notifyGroupViewed(groupId: group.id)
            reload()
            cancellable = ChatEvents.subject.sink { chatId in
                if chatId == group.id { reload() }
            }
        }
        .onDisappear {
            ChatVisibility.clearVisible(group.id)
            ChatEvents.notifyChatChanged(group.id)
        }
        .sheet(isPresented: $showDetails) {
            GroupDetailsView(group: group, identity: identity) {
                showDetails = false
                confirmDelete = true
            }
        }
        .alert("Delete group?", isPresented: $confirmDelete) {
            Button("Delete", role: .destructive) {
                try? store.deleteGroup(groupId: group.id)
                dismiss()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Removes this group and its messages from this device. Other members keep their copy.")
        }
    }

    private func reload() {
        messages = (try? store.messagesForChat(chatId: group.id)) ?? []
        MeshController.shared.notifyGroupViewed(groupId: group.id)
    }

    private func senderName(_ userId: Data) -> String {
        if userId == identity.userId { return "You" }
        if let contact = try? store.getContact(userId: userId) {
            return ChatListLogic.displayNameOrId(
                name: contact.name,
                displayId: formatUserId(userId: userId)
            )
        }
        return formatUserId(userId: userId)
    }

    private func messageId(_ message: StoredMessage) -> String {
        "\(UserIdHex.encode(message.senderUserId))-\(message.lamport)-\(message.kind)"
    }

    private func isNewDay(_ index: Int) -> Bool {
        guard index > 0 else { return true }
        let calendar = Calendar.current
        let current = Date(timeIntervalSince1970: TimeInterval(visibleMessages[index].timestamp) / 1_000)
        let previous = Date(timeIntervalSince1970: TimeInterval(visibleMessages[index - 1].timestamp) / 1_000)
        return !calendar.isDate(current, inSameDayAs: previous)
    }

    private func dayLabel(_ timestampMs: Int64) -> String {
        let formatter = DateFormatter()
        formatter.dateFormat = "MMMM d, yyyy"
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1_000))
    }
}

private struct GroupMessageRow: View {
    let message: StoredMessage
    let isOwn: Bool
    let senderName: String
    let groupName: String

    var body: some View {
        if message.kind == ProtocolKind.groupInvite {
            Text(ChatListLogic.previewText(message, groupName: groupName))
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
        } else {
            HStack {
                if isOwn { Spacer(minLength: 40) }
                VStack(alignment: isOwn ? .trailing : .leading, spacing: 3) {
                    if !isOwn {
                        Text(senderName)
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(Color.accentColor)
                            .padding(.horizontal, 8)
                    }
                    Text(String(data: message.payload, encoding: .utf8) ?? "")
                        .padding(10)
                        .foregroundStyle(isOwn ? Color.white : Color.primary)
                        .background(
                            RoundedRectangle(cornerRadius: 18, style: .continuous)
                                .fill(isOwn ? Color.accentColor : Color(.secondarySystemBackground))
                        )
                    Text(timeLabel(message.timestamp))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                if !isOwn { Spacer(minLength: 40) }
            }
        }
    }

    private func timeLabel(_ timestampMs: Int64) -> String {
        let formatter = DateFormatter()
        formatter.dateFormat = "h:mm a"
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1_000))
    }
}

private struct GroupDetailsView: View {
    let group: Group
    let identity: Identity
    let onDelete: () -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section {
                    HStack {
                        AvatarView(userId: group.id, name: group.name, size: 56)
                        VStack(alignment: .leading) {
                            Text(group.name).font(.title3.weight(.semibold))
                            Text("\(group.memberUserIds.count) members")
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                Section("Members") {
                    ForEach(group.memberUserIds, id: \.self) { memberId in
                        HStack {
                            AvatarView(userId: memberId, name: memberName(memberId), size: 36)
                            Text(memberName(memberId))
                        }
                    }
                }
                Section {
                    Button("Leave / delete group", role: .destructive, action: onDelete)
                }
            }
            .navigationTitle("Group info")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private func memberName(_ userId: Data) -> String {
        if userId == identity.userId { return "You" }
        if let contact = try? AppStore.get().getContact(userId: userId) {
            return ChatListLogic.displayNameOrId(
                name: contact.name,
                displayId: formatUserId(userId: userId)
            )
        }
        return formatUserId(userId: userId)
    }
}
