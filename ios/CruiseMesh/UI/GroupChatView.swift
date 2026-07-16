import Combine
import SwiftUI

/// Group chat thread (DESIGN.md §6.5). Local `chat_id` is the group id.
/// Group wire receipts are deferred — no ✓/✓✓ ticks yet.
struct GroupChatView: View {
    let group: Group
    let identity: Identity

    @Environment(\.dismiss) private var dismiss
    @State private var messages: [StoredMessage] = []
    @State private var contactsById: [Data: Contact] = [:]
    @State private var draft = ""
    @State private var showDetails = false
    @State private var confirmDelete = false
    @State private var cancellable: AnyCancellable?
    @State private var replyingTo: StoredMessage?
    @State private var replyMetadata: [String: MessageReplyMetadata] = [:]

    private let store = AppStore.get()
    private var sender: GroupSender { GroupSender(store: store, identity: identity) }

    private var visible: [StoredMessage] {
        messages.filter { isVisibleChatKind($0.kind) }
    }

    private var reactions: [String: [ReactionSummary]] {
        reactionSummariesByTarget(messages: messages, ownUserId: identity.userId)
    }

    private var replyingToPreview: QuotedMessagePreview? {
        replyingTo.map { target in
            quotedMessagePreview(target: target) { message in
                senderName(message.senderUserId)
            }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            GeometryReader { geo in
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 2) {
                            ForEach(Array(visible.enumerated()), id: \.element.stableGroupRowId) { index, message in
                                GroupMessageRow(
                                    message: message,
                                    isOwn: message.senderUserId == identity.userId,
                                    groupName: group.name,
                                    senderLabel: senderLabel(at: index),
                                    contactColor: ChatListLogic.avatarHueAndInitials(
                                        userId: message.senderUserId,
                                        name: senderName(message.senderUserId),
                                        displayId: formatUserId(userId: message.senderUserId)
                                    ).0,
                                    quoted: replyMetadata[replyMessageKey(message)]?.quoted,
                                    canReply: replyMetadata[replyMessageKey(message)]?.msgId != nil,
                                    reactions: reactions[MessageTarget(
                                        senderUserId: message.senderUserId,
                                        lamport: message.lamport,
                                        kind: message.kind
                                    ).stableKey] ?? [],
                                    onReact: { emoji in
                                        sendReaction(to: message, emoji: emoji)
                                    },
                                    onReply: {
                                        replyingTo = message
                                    },
                                    onQuotedTap: { target in
                                        withAnimation {
                                            proxy.scrollTo(messageId(target), anchor: .center)
                                        }
                                    }
                                )
                                .id(messageId(message))
                            }
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .frame(minHeight: geo.size.height, alignment: .bottom)
                    }
                    .scrollDismissesKeyboard(.interactively)
                    .onChange(of: visible.count) { _ in
                        scrollToLatest(proxy: proxy)
                    }
                    .onAppear {
                        scrollToLatest(proxy: proxy, animated: false)
                    }
                }
            }

            VStack(spacing: 8) {
                if let replyingToPreview {
                    ReplyComposerPreview(preview: replyingToPreview) {
                        replyingTo = nil
                    }
                }
                HStack(alignment: .center, spacing: 8) {
                    TextField("Message", text: $draft, axis: .vertical)
                        .lineLimit(1...4)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .background(
                            Capsule(style: .continuous)
                                .fill(Color(uiColor: .secondarySystemBackground))
                        )

                    Button {
                        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                        guard !text.isEmpty else { return }
                        let replyToMsgId = replyingTo.flatMap {
                            replyMetadata[replyMessageKey($0)]?.msgId
                        }
                        sender.sendText(group: group, text: text, replyToMsgId: replyToMsgId)
                        draft = ""
                        replyingTo = nil
                        reload()
                    } label: {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.system(size: 32, weight: .semibold))
                    }
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .opacity(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? 0.36 : 1)
                    .accessibilityLabel("Send")
                }
            }
            .padding(12)
            .background(.bar)
        }
        .navigationTitle(group.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Button { showDetails = true } label: {
                    HStack {
                        AvatarView(userId: group.id, name: group.name, size: 32, isGroup: true)
                        VStack(alignment: .leading) {
                            Text(group.name)
                                .font(.headline)
                            Text("\(group.memberUserIds.count) members · tap for details")
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
            MeshController.shared.notifyChatViewed(chatId: group.id)
            loadContacts()
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
            GroupDetailsSheet(
                group: group,
                nameForMember: senderName
            ) {
                showDetails = false
                confirmDelete = true
            }
        }
        .alert("Delete \(group.name)?", isPresented: $confirmDelete) {
            Button("Delete", role: .destructive) {
                _ = try? store.deleteGroup(groupId: group.id)
                dismiss()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Removes this group and its message history from this device. Other members keep their copy.")
        }
    }

    private func senderName(_ userId: Data) -> String {
        if userId == identity.userId { return "You" }
        if let contact = contactsById[userId], !contact.name.isEmpty {
            return contact.name
        }
        return formatUserId(userId: userId)
    }

    private func messageId(_ message: StoredMessage) -> String {
        "\(UserIdHex.encode(message.senderUserId))-\(message.lamport)-\(message.kind)"
    }

    private func scrollToLatest(proxy: ScrollViewProxy, animated: Bool = true) {
        guard let last = visible.last else { return }
        let id = messageId(last)
        if animated {
            withAnimation { proxy.scrollTo(id, anchor: .bottom) }
        } else {
            proxy.scrollTo(id, anchor: .bottom)
        }
    }

    /// Show a sender label above a non-own message when it starts a new run
    /// (different sender than the previous visible message).
    private func senderLabel(at index: Int) -> String? {
        let message = visible[index]
        guard message.senderUserId != identity.userId else { return nil }
        if index > 0, visible[index - 1].senderUserId == message.senderUserId {
            return nil
        }
        return senderName(message.senderUserId)
    }

    private func loadContacts() {
        let contacts = (try? store.listContacts()) ?? []
        contactsById = Dictionary(uniqueKeysWithValues: contacts.map { ($0.userId, $0) })
    }

    private func reload() {
        let loadedMessages = (try? store.messagesForChat(chatId: group.id)) ?? []
        messages = loadedMessages
        replyMetadata = loadMessageReplyMetadata(
            store: store,
            messages: loadedMessages.filter { isVisibleChatKind($0.kind) }
        ) { message in
            senderName(message.senderUserId)
        }
        MeshController.shared.notifyChatViewed(chatId: group.id)
    }

    private func sendReaction(to message: StoredMessage, emoji: String) {
        let target = MessageTarget(
            senderUserId: message.senderUserId,
            lamport: message.lamport,
            kind: message.kind
        )
        let existingOwn = reactions[target.stableKey]?.contains {
            $0.emoji == emoji && $0.reactedByOwnUser
        } ?? false
        sender.sendReaction(group: group, target: target, emoji: existingOwn ? "" : emoji)
        reload()
    }
}

private struct GroupMessageRow: View {
    let message: StoredMessage
    let isOwn: Bool
    let groupName: String
    let senderLabel: String?
    let contactColor: Color
    let quoted: QuotedMessagePreview?
    let canReply: Bool
    let reactions: [ReactionSummary]
    let onReact: (String) -> Void
    let onReply: () -> Void
    let onQuotedTap: (StoredMessage) -> Void

    var body: some View {
        if message.kind == ProtocolKind.groupInvite {
            Text(ChatListLogic.previewText(message, groupName: groupName))
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 6)
        } else {
            HStack {
                if isOwn { Spacer(minLength: 40) }
                VStack(alignment: isOwn ? .trailing : .leading, spacing: 2) {
                    if let senderLabel {
                        Text(senderLabel)
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(contactColor)
                            .padding(.leading, 6)
                    }
                    VStack(alignment: .leading, spacing: 6) {
                        if let quoted {
                            QuotedMessageBlock(
                                preview: quoted,
                                accentColor: isOwn ? .white : contactColor,
                                contentColor: isOwn ? .white : .primary,
                                onTap: quoted.target.map { target in
                                    { onQuotedTap(target) }
                                }
                            )
                        }
                        Text(String(data: message.payload, encoding: .utf8) ?? "")
                    }
                        .padding(10)
                        .background(
                            RoundedRectangle(cornerRadius: 18, style: .continuous)
                                .fill(isOwn ? Color.accentColor : contactColor.opacity(0.24))
                        )
                        .foregroundStyle(isOwn ? Color.white : Color.primary)
                        .contextMenu {
                            if canReply {
                                Button(action: onReply) {
                                    Label("Reply", systemImage: "arrowshape.turn.up.left")
                                }
                            }
                            ForEach(reactionChoices, id: \.self) { emoji in
                                Button(emoji) { onReact(emoji) }
                            }
                        }
                    if !reactions.isEmpty {
                        ReactionPillRow(reactions: reactions, isOwn: isOwn, onReact: onReact)
                    }
                    Text(timeLabel(message.timestamp))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                if !isOwn { Spacer(minLength: 40) }
            }
            .padding(.vertical, 2)
        }
    }

    private func timeLabel(_ ms: Int64) -> String {
        let f = DateFormatter()
        f.dateFormat = "h:mm a"
        f.locale = .current
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }
}

private struct GroupDetailsSheet: View {
    let group: Group
    let nameForMember: (Data) -> String
    let onDelete: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section("Group") {
                    LabeledContent("Name", value: group.name)
                    LabeledContent("Members", value: "\(group.memberUserIds.count)")
                }
                Section("Members") {
                    ForEach(group.memberUserIds, id: \.self) { memberId in
                        Text(nameForMember(memberId))
                    }
                }
                Section {
                    Button("Leave / delete group", role: .destructive, action: onDelete)
                }
            }
            .navigationTitle("Details")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}

private extension StoredMessage {
    var stableGroupRowId: String {
        "\(UserIdHex.encode(senderUserId))-\(lamport)-\(kind)"
    }
}
