import Combine
import SwiftUI

struct ChatSummary: Identifiable {
    var id: Data { chatId }
    let chatId: Data
    let title: String
    let isGroup: Bool
    let contact: Contact?
    let group: Group?
    let lastMessage: StoredMessage?
    let unreadCount: Int
    let ownDeliveredThrough: UInt64
    let ownReadThrough: UInt64
    let avatarData: Data?
    let draft: String
    let isMuted: Bool
}

/// Navigation target for the chat list — a 1:1 contact chat or a group chat.
enum ChatRoute: Hashable {
    case contact(Data)
    case group(Data)
}

struct ChatListView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @ObservedObject private var bluetooth = BluetoothAccess.shared
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @ObservedObject private var connectivityClock = ConnectivityClock.shared
    @State private var summaries: [ChatSummary] = []
    @State private var showFriends = false
    @State private var showProfile = false
    @State private var showNewGroup = false
    @State private var showMeshHelp = false
    @State private var cancellable: AnyCancellable?
    @State private var bluetoothAudioWarningDismissed = false
    @State private var publishedFriendDirectory = false
    @State private var showStoreRecoveryNotice = false
    @State private var path = NavigationPath()
    @AppStorage("hideBluetoothAudioWarning") private var hideBluetoothAudioWarning = false

    private var connectivityWarning: ConnectivityWarning? {
        if bluetooth.isAuthorizationBlocked {
            return ConnectivityWarning(
                title: "Bluetooth permission required — mesh is off",
                body: "Without Bluetooth access, CruiseMesh cannot scan, connect, send, or receive messages. The app will not work as designed until you allow Bluetooth in Settings.",
                actionLabel: "Open Settings",
                severity: .blocking
            )
        }
        if bluetooth.isRadioOff {
            return ConnectivityWarning(
                title: "Bluetooth is off",
                body: "CruiseMesh needs Bluetooth on to find nearby phones and deliver messages. Turn it on to use the mesh.",
                actionLabel: "Open Settings",
                severity: .blocking
            )
        }
        if case .pausedForBluetoothAudio = runtime.state,
           !bluetoothAudioWarningDismissed,
           !hideBluetoothAudioWarning {
            return ConnectivityWarning(
                title: "Bluetooth audio connected",
                body: "The mesh may pause or slow while wireless audio is active. Watch for delayed delivery.",
                actionLabel: "Dismiss",
                secondaryActionLabel: "Don't show this again",
                severity: .caution
            )
        }
        return nil
    }

    var body: some View {
        NavigationStack(path: $path) {
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
                        Button("Add a friend") { showFriends = true }
                            .buttonStyle(.borderedProminent)
                            .padding(.top, 6)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    List(summaries) { summary in
                        NavigationLink(value: route(for: summary)) {
                            ChatRowView(
                                summary: summary,
                                ownUserId: identity.userId,
                                reachability: summary.isGroup ? nil : connectivity.level(
                                    for: summary.chatId,
                                    nowMs: connectivityClock.nowMs
                                )
                            )
                        }
                        .swipeActions {
                            Button(role: .destructive) {
                                delete(summary)
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle("CruiseMesh")
            .navigationDestination(for: ChatRoute.self) { route in
                switch route {
                case .contact(let userId):
                    if let contact = try? AppStore.get().getContact(userId: userId) {
                        ChatView(contact: contact, identity: identity)
                    }
                case .group(let groupId):
                    if let group = try? AppStore.get().getGroup(groupId: groupId) {
                        GroupChatView(group: group, identity: identity)
                    }
                }
            }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { showProfile = true } label: {
                        AvatarView(
                            userId: identity.userId,
                            name: appModel.displayName,
                            size: 32,
                            photo: ProfilePhotoStore.loadAvatarImage()
                        )
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button("New group") { showNewGroup = true }
                        Button("Friends") { showFriends = true }
                        Button("Mesh status") { showMeshHelp = true }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                }
            }
            .safeAreaInset(edge: .top) {
                VStack(spacing: 0) {
                    if showStoreRecoveryNotice {
                        HStack(alignment: .top, spacing: 10) {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .foregroundStyle(.orange)
                            Text("Message history could not be read and was reset.")
                                .font(.caption)
                                .fixedSize(horizontal: false, vertical: true)
                            Spacer(minLength: 0)
                            Button("Dismiss") { showStoreRecoveryNotice = false }
                                .font(.caption.weight(.semibold))
                        }
                        .padding(10)
                        .background(Color.orange.opacity(0.16))
                    }
                    if let warning = connectivityWarning {
                        ConnectivityWarningBanner(
                            warning: warning,
                            onAction: {
                                if warning.severity == .blocking {
                                    bluetooth.openSystemSettings()
                                } else {
                                    bluetoothAudioWarningDismissed = true
                                }
                            },
                            onSecondaryAction: warning.severity == .caution ? {
                                hideBluetoothAudioWarning = true
                                bluetoothAudioWarningDismissed = true
                            } : nil
                        )
                    }
                    MeshStatusPill {
                        if bluetooth.isAuthorizationBlocked || bluetooth.isRadioOff {
                            bluetooth.openSystemSettings()
                        } else if case .stopped = runtime.state {
                            appModel.startMesh()
                        } else {
                            showMeshHelp = true
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                }
            }
            .sheet(isPresented: $showFriends) {
                FriendsView(
                    identity: identity,
                    appModel: appModel,
                    initialToken: appModel.pendingFriendToken
                ) {
                    showFriends = false
                    appModel.pendingFriendToken = nil
                    reload()
                }
            }
            .sheet(isPresented: $showNewGroup) {
                NewGroupView(identity: identity) { group in
                    showNewGroup = false
                    reload()
                    path.append(ChatRoute.group(group.id))
                }
            }
            .sheet(isPresented: $showProfile) {
                ProfileView(identity: identity, appModel: appModel)
            }
            .sheet(isPresented: $showMeshHelp) {
                MeshStatusSheet(appModel: appModel)
            }
            .onAppear {
                reload()
                cancellable = ChatEvents.subject.sink { _ in reload() }
                appModel.startMeshIfEnabled()
                if AppStore.consumeRecoveryNotice() { showStoreRecoveryNotice = true }
                if !publishedFriendDirectory {
                    publishedFriendDirectory = true
                    ProfileSyncSender.queueToAllContacts(
                        store: AppStore.get(),
                        identity: identity,
                        displayName: appModel.displayName,
                        epoch: ProfileStore.loadOwnAvatarEpoch()
                    )
                    FriendDirectorySender.queueToAllContacts(store: AppStore.get(), identity: identity)
                }
                if appModel.pendingFriendToken != nil { showFriends = true }
            }
            .onChange(of: runtime.state) { state in
                if case .pausedForBluetoothAudio = state {
                    return
                }
                bluetoothAudioWarningDismissed = false
            }
            .onChange(of: appModel.pendingFriendToken) { token in
                if token != nil { showFriends = true }
            }
            .onReceive(NotificationOpenEvents.subject) { event in
                let (chatId, isGroup) = event
                path.append(isGroup ? ChatRoute.group(chatId) : ChatRoute.contact(chatId))
            }
        }
    }

    private func route(for summary: ChatSummary) -> ChatRoute {
        summary.isGroup ? .group(summary.chatId) : .contact(summary.chatId)
    }

    private func delete(_ summary: ChatSummary) {
        if summary.isGroup {
            _ = try? AppStore.get().deleteGroup(groupId: summary.chatId)
        } else {
            try? AppStore.get().deleteContact(userId: summary.chatId)
            FriendDirectorySender.queueToAllContacts(store: AppStore.get(), identity: identity)
        }
        reload()
    }

    private func reload() {
        let store = AppStore.get()
        let contacts = (try? store.listContacts()) ?? []
        let direct: [ChatSummary] = contacts.map { c in
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
            let unread = (try? store.semanticUnreadCount(
                chatId: c.userId,
                ownUserId: identity.userId
            )) ?? 0
            return ChatSummary(
                chatId: c.userId,
                title: coreContactDisplayName(contact: c),
                isGroup: false,
                contact: c,
                group: nil,
                lastMessage: ChatListLogic.lastVisibleMessage(messages),
                unreadCount: Int(unread),
                ownDeliveredThrough: deliveredThrough,
                ownReadThrough: readThrough,
                avatarData: (try? store.contactAvatar(userId: c.userId)) ?? nil,
                draft: DraftStore.load(chatId: c.userId),
                isMuted: ChatMuteStore.isMuted(c.userId)
            )
        }
        let groups = (try? store.listGroups()) ?? []
        let groupSummaries: [ChatSummary] = groups.map { g in
            let messages = (try? store.messagesForChat(chatId: g.id)) ?? []
            let unread = (try? store.semanticUnreadCount(
                chatId: g.id,
                ownUserId: identity.userId
            )) ?? 0
            return ChatSummary(
                chatId: g.id,
                title: g.name,
                isGroup: true,
                contact: nil,
                group: g,
                lastMessage: ChatListLogic.lastVisibleMessage(messages),
                unreadCount: Int(unread),
                ownDeliveredThrough: 0,
                ownReadThrough: 0,
                avatarData: nil,
                draft: DraftStore.load(chatId: g.id),
                isMuted: ChatMuteStore.isMuted(g.id)
            )
        }
        summaries = (direct + groupSummaries)
            .sorted { ($0.lastMessage?.timestamp ?? 0) > ($1.lastMessage?.timestamp ?? 0) }
    }
}

private struct ChatRowView: View {
    let summary: ChatSummary
    let ownUserId: Data
    let reachability: ReachabilityLevel?

    private var displayName: String {
        summary.isGroup
            ? summary.title
            : ChatListLogic.displayNameOrId(name: summary.title, displayId: formatUserId(userId: summary.chatId))
    }

    var body: some View {
        HStack(spacing: 12) {
            AvatarView(
                userId: summary.chatId,
                name: summary.title,
                size: 48,
                photo: summary.avatarData.flatMap { UIImage(data: $0) },
                isGroup: summary.isGroup,
                reachability: reachability
            )
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(displayName)
                        .font(.headline)
                        .fontWeight(summary.unreadCount > 0 ? .bold : .semibold)
                    Spacer()
                    if summary.isMuted {
                        Image(systemName: "bell.slash.fill")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    if let last = summary.lastMessage {
                        Text(ChatListLogic.formatRelativeTime(timestampMs: last.timestamp))
                            .font(.caption)
                            .foregroundStyle(summary.unreadCount > 0 ? Color.accentColor : .secondary)
                    }
                }
                HStack {
                    if !summary.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        Text("Draft: \(summary.draft)")
                            .font(.subheadline)
                            .foregroundStyle(.red)
                            .lineLimit(1)
                    } else if let last = summary.lastMessage {
                        let isOwn = last.senderUserId == ownUserId
                        if isOwn && !summary.isGroup {
                            SignalTickView(status: tickStatusFor(
                                lamport: last.lamport,
                                deliveredThrough: summary.ownDeliveredThrough,
                                readThrough: summary.ownReadThrough
                            ))
                        }
                        Text((isOwn ? "You: " : "") + ChatListLogic.previewText(last, groupName: summary.isGroup ? summary.title : nil))
                            .font(.subheadline)
                            .foregroundStyle(summary.unreadCount > 0 ? .primary : .secondary)
                            .lineLimit(1)
                    }
                    Spacer()
                    if summary.unreadCount > 0 {
                        Text(unreadBadgeText(summary.unreadCount))
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

private struct MeshStatusSheet: View {
    @ObservedObject var appModel: AppModel
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @Environment(\.dismiss) private var dismiss
    // How many other people's messages this phone is carrying (DTN mule
    // queue). Snapshot when the sheet appears; `carriedLen` is the read-only
    // core accessor.
    @State private var carriedCount: UInt64 = 0

    private var meshOn: Binding<Bool> {
        Binding(
            get: {
                if case .stopped = runtime.state { return false }
                return true
            },
            set: { enabled in
                if enabled {
                    appModel.startMesh()
                } else {
                    appModel.stopMesh()
                }
            }
        )
    }

    var body: some View {
        NavigationStack {
            List {
                Section("Current status") {
                    HStack {
                        StatusDot(color: dotColor(for: runtime.state))
                        Text(runtime.pillText)
                    }
                    Toggle("Mesh running", isOn: meshOn)
                }
                Section("Reachability dots") {
                    LegendRow(color: .green, label: "Nearby", detail: "Direct Bluetooth link")
                    LegendRow(color: .blue, label: "Relay", detail: "Online through relay")
                    LegendRow(color: .orange, label: "Recent", detail: "Seen recently")
                    LegendRow(color: .orange, hollow: true, label: "Carried", detail: "Nearby phones may carry messages")
                    LegendRow(color: .gray, label: "Offline", detail: "No recent route")
                }
                if carriedCount > 0 {
                    Section {
                        Text(carriedCount == 1
                            ? "Carrying 1 message for others"
                            : "Carrying \(carriedCount) messages for others")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
                Section {
                    Text("Open the app when you sit down with family so phones can sync over Bluetooth.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .onAppear { carriedCount = (try? AppStore.get().carriedLen()) ?? 0 }
            .navigationTitle("Mesh status")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private func dotColor(for state: MeshRuntimeState) -> Color {
        switch state {
        case .stopped: return .gray
        case .starting: return .orange
        case .meshing: return .green
        case .pausedForBluetoothAudio: return .yellow
        case .syncingViaRelay: return .blue
        }
    }
}

private struct LegendRow: View {
    let color: Color
    var hollow = false
    let label: String
    let detail: String

    var body: some View {
        HStack(spacing: 12) {
            StatusDot(color: color, hollow: hollow)
            VStack(alignment: .leading, spacing: 2) {
                Text(label)
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

private struct StatusDot: View {
    let color: Color
    var hollow = false

    var body: some View {
        ZStack {
            Circle()
                .fill(hollow ? Color.clear : color)
            Circle()
                .stroke(color, lineWidth: hollow ? 2 : 0)
        }
        .frame(width: 12, height: 12)
    }
}

private func unreadBadgeText(_ count: Int) -> String {
    count > 99 ? "99+" : "\(max(0, count))"
}
