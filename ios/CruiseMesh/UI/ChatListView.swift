import Combine
import SwiftUI

struct ChatSummary: Identifiable {
    var id: Data { contact.userId }
    let contact: Contact
    let lastMessage: StoredMessage?
    let unreadCount: Int
    let ownDeliveredThrough: UInt64
    let ownReadThrough: UInt64
}

struct ChatListView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @State private var summaries: [ChatSummary] = []
    @State private var showFriends = false
    @State private var showProfile = false
    @State private var showMeshHelp = false
    @State private var cancellable: AnyCancellable?

    var body: some View {
        NavigationStack {
            Group {
                if summaries.isEmpty {
                    ContentUnavailableView(
                        "No chats yet",
                        systemImage: "bubble.left.and.bubble.right",
                        description: Text("Add a friend with a QR code to start messaging on the mesh.")
                    )
                } else {
                    List(summaries) { summary in
                        NavigationLink(value: UserIdHex.encode(summary.contact.userId)) {
                            ChatRowView(summary: summary, ownUserId: identity.userId)
                        }
                        .swipeActions {
                            Button(role: .destructive) {
                                try? AppStore.get().deleteContact(userId: summary.contact.userId)
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
                if let contact = try? AppStore.get().getContact(userId: try UserIdHex.decode(hex)) {
                    ChatView(contact: contact, identity: identity)
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
                Button { showFriends = true } label: {
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
            .alert("Mesh", isPresented: $showMeshHelp) {
                Button("Start mesh") { appModel.startMesh() }
                Button("Stop mesh", role: .destructive) { MeshController.shared.stop() }
                Button("OK", role: .cancel) {}
            } message: {
                Text(MeshRuntimeStatus.shared.pillText + "\n\nOpen the app when you sit down with family so phones can sync over Bluetooth.")
            }
            .onAppear {
                reload()
                cancellable = ChatEvents.subject.sink { _ in reload() }
                appModel.startMesh()
            }
        }
    }

    private func reload() {
        let store = AppStore.get()
        let contacts = (try? store.listContacts()) ?? []
        summaries = contacts.map { c in
            let messages = (try? store.messagesForChat(chatId: c.userId)) ?? []
            let readThrough = (try? store.receiptThrough(
                chatId: c.userId,
                senderUserId: identity.userId,
                receiptType: ReceiptType.read
            )) ?? 0
            let deliveredThrough = (try? store.receiptThrough(
                chatId: c.userId,
                senderUserId: identity.userId,
                receiptType: ReceiptType.delivered
            )) ?? 0
            return ChatSummary(
                contact: c,
                lastMessage: ChatListLogic.lastVisibleMessage(messages),
                unreadCount: ChatListLogic.computeUnread(
                    messages: messages,
                    ownUserId: identity.userId,
                    readThrough: readThrough
                ),
                ownDeliveredThrough: deliveredThrough,
                ownReadThrough: readThrough
            )
        }
        .sorted { ($0.lastMessage?.timestamp ?? 0) > ($1.lastMessage?.timestamp ?? 0) }
    }
}

private struct ChatRowView: View {
    let summary: ChatSummary
    let ownUserId: Data

    var body: some View {
        HStack(spacing: 12) {
            AvatarView(userId: summary.contact.userId, name: summary.contact.name, size: 48)
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(ChatListLogic.displayNameOrId(
                        name: summary.contact.name,
                        displayId: formatUserId(userId: summary.contact.userId)
                    ))
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
                        if isOwn {
                            SignalTickView(status: tickStatusFor(
                                lamport: last.lamport,
                                deliveredThrough: summary.ownDeliveredThrough,
                                readThrough: summary.ownReadThrough
                            ))
                        }
                        Text((isOwn ? "You: " : "") + ChatListLogic.previewText(last))
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
