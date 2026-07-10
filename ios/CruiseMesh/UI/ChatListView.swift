import Combine
import SwiftUI

struct ChatSummary: Identifiable {
    var id: Data { chatId }
    let chatId: Data
    let title: String
    let group: Group?
    let lastMessage: StoredMessage?
    let unreadCount: Int
    let ownDeliveredThrough: UInt64
    let ownReadThrough: UInt64

    var isGroup: Bool { group != nil }
    var navigationValue: String {
        "\(isGroup ? "group" : "contact"):\(UserIdHex.encode(chatId))"
    }
}

struct ChatListView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @State private var summaries: [ChatSummary] = []
    @State private var showFriends = false
    @State private var showProfile = false
    @State private var showNewGroup = false
    @State private var showMeshHelp = false
    @State private var cancellable: AnyCancellable?

    var body: some View {
        NavigationStack {
            SwiftUI.Group {
                if summaries.isEmpty {
                    VStack(spacing: 12) {
                        Image(systemName: "bubble.left.and.bubble.right")
                            .font(.system(size: 44))
                            .foregroundStyle(.secondary)
                        Text("No chats yet")
                            .font(.title2.weight(.semibold))
                        Text("Add a friend with a QR code to start messaging on the mesh.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 32)
                    }
                } else {
                    List(summaries) { summary in
                        NavigationLink(value: summary.navigationValue) {
                            ChatRowView(summary: summary, ownUserId: identity.userId)
                        }
                        .swipeActions {
                            Button(role: .destructive) {
                                if summary.isGroup {
                                    try? AppStore.get().deleteGroup(groupId: summary.chatId)
                                } else {
                                    try? AppStore.get().deleteContact(userId: summary.chatId)
                                }
                                reload()
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle("CruiseMesh")
            .navigationDestination(for: String.self) { hex in
                let pieces = hex.split(separator: ":", maxSplits: 1).map(String.init)
                if pieces.count == 2, let chatId = try? UserIdHex.decode(pieces[1]) {
                    if pieces[0] == "group",
                       let group = try? AppStore.get().getGroup(groupId: chatId) {
                        GroupChatView(group: group, identity: identity)
                    } else if pieces[0] == "contact",
                              let contact = try? AppStore.get().getContact(userId: chatId) {
                        ChatView(contact: contact, identity: identity)
                    }
                }
            }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { showProfile = true } label: {
                        AvatarView(userId: identity.userId, name: appModel.displayName, size: 32)
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button("Friends") { showFriends = true }
                        Button("New group") { showNewGroup = true }
                        Button("Mesh status") { showMeshHelp = true }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                }
            }
            .safeAreaInset(edge: .top) {
                MeshStatusPill {
                    if case .stopped = MeshRuntimeStatus.shared.state {
                        appModel.startMesh()
                    } else {
                        showMeshHelp = true
                    }
                }
                .padding(.bottom, 4)
            }
            .overlay(alignment: .bottomTrailing) {
                Menu {
                    Button("New message") { showFriends = true }
                    Button("New group") { showNewGroup = true }
                } label: {
                    Image(systemName: "square.and.pencil")
                        .font(.title2.weight(.semibold))
                        .foregroundStyle(.white)
                        .frame(width: 56, height: 56)
                        .background(Circle().fill(Color.accentColor))
                        .shadow(radius: 4)
                }
                .padding(20)
                .accessibilityLabel("New chat")
            }
            .sheet(isPresented: $showFriends) {
                FriendsView(identity: identity, appModel: appModel) {
                    showFriends = false
                    reload()
                }
            }
            .sheet(isPresented: $showProfile) {
                ProfileView(identity: identity, appModel: appModel)
            }
            .sheet(isPresented: $showNewGroup) {
                NewGroupView(
                    identity: identity,
                    contacts: (try? AppStore.get().listContacts()) ?? []
                ) { _ in
                    reload()
                }
            }
            .alert("Mesh", isPresented: $showMeshHelp) {
                Button("Start mesh") { appModel.startMesh() }
                Button("Stop mesh", role: .destructive) { appModel.stopMesh() }
                Button("OK", role: .cancel) {}
            } message: {
                Text(MeshRuntimeStatus.shared.pillText + "\n\nOpen the app when you sit down with family so phones can sync over Bluetooth.")
            }
            .onAppear {
                reload()
                cancellable = ChatEvents.subject.sink { _ in reload() }
                appModel.startMeshIfEnabled()
            }
        }
    }

    private func reload() {
        let store = AppStore.get()
        let contacts = (try? store.listContacts()) ?? []
        let directSummaries = contacts.map { c in
            let messages = (try? store.messagesForChat(chatId: c.userId)) ?? []
            let readThrough = (try? store.receiptThrough(
                chatId: c.userId,
                senderUserId: identity.userId,
                receiptType: ReceiptType.read
            )) ?? 0
            let localReadThrough = (try? store.outgoingReceiptThrough(
                chatId: c.userId,
                senderUserId: c.userId,
                receiptType: ReceiptType.read
            )) ?? 0
            let deliveredThrough = (try? store.receiptThrough(
                chatId: c.userId,
                senderUserId: identity.userId,
                receiptType: ReceiptType.delivered
            )) ?? 0
            return ChatSummary(
                chatId: c.userId,
                title: ChatListLogic.displayNameOrId(
                    name: c.name,
                    displayId: formatUserId(userId: c.userId)
                ),
                group: nil,
                lastMessage: ChatListLogic.lastVisibleMessage(messages),
                unreadCount: ChatListLogic.computeUnread(
                    messages: messages,
                    ownUserId: identity.userId,
                    readThrough: localReadThrough
                ),
                ownDeliveredThrough: deliveredThrough,
                ownReadThrough: readThrough
            )
        }
        let groups = (try? store.listGroups()) ?? []
        let groupSummaries = groups.map { group in
            let messages = (try? store.messagesForChat(chatId: group.id)) ?? []
            let unread = ChatListLogic.computeGroupUnread(
                messages: messages,
                ownUserId: identity.userId
            ) { senderUserId in
                (try? store.outgoingReceiptThrough(
                    chatId: group.id,
                    senderUserId: senderUserId,
                    receiptType: ReceiptType.read
                )) ?? 0
            }
            return ChatSummary(
                chatId: group.id,
                title: group.name,
                group: group,
                lastMessage: ChatListLogic.lastVisibleMessage(messages),
                unreadCount: unread,
                ownDeliveredThrough: 0,
                ownReadThrough: 0
            )
        }
        summaries = (directSummaries + groupSummaries)
        .sorted { ($0.lastMessage?.timestamp ?? 0) > ($1.lastMessage?.timestamp ?? 0) }
    }
}

private struct ChatRowView: View {
    let summary: ChatSummary
    let ownUserId: Data

    var body: some View {
        HStack(spacing: 12) {
            AvatarView(userId: summary.chatId, name: summary.title, size: 48)
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(summary.title)
                    .font(.headline)
                    .fontWeight(summary.unreadCount > 0 ? .bold : .semibold)
                    Spacer()
                    if let last = summary.lastMessage {
                        Text(ChatListLogic.formatRelativeTime(timestampMs: last.timestamp))
                            .font(.caption)
                            .foregroundStyle(summary.unreadCount > 0 ? Color.accentColor : .secondary)
                    }
                }
                HStack {
                    if let last = summary.lastMessage {
                        let isOwn = last.senderUserId == ownUserId
                        if isOwn && !summary.isGroup {
                            SignalTickView(status: tickStatusFor(
                                lamport: last.lamport,
                                deliveredThrough: summary.ownDeliveredThrough,
                                readThrough: summary.ownReadThrough
                            ))
                        }
                        Text((isOwn ? "You: " : "") + ChatListLogic.previewText(
                            last,
                            groupName: summary.group?.name
                        ))
                            .font(.subheadline)
                            .foregroundStyle(summary.unreadCount > 0 ? .primary : .secondary)
                            .lineLimit(1)
                    }
                    Spacer()
                    if summary.unreadCount > 0 {
                        Text("\(summary.unreadCount)")
                            .font(.caption2.bold())
                            .foregroundStyle(.white)
                            .padding(.horizontal, 7)
                            .padding(.vertical, 3)
                            .background(Capsule().fill(Color.accentColor))
                    }
                }
            }
        }
        .padding(.vertical, 4)
    }
}
