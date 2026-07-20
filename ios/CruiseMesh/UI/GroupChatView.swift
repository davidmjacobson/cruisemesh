import AVFoundation
import Combine
import PhotosUI
import SwiftUI
import UIKit

/// Group chat thread (DESIGN.md §6.5). Local `chat_id` is the group id.
/// Group wire receipts are deferred — no ✓/✓✓ ticks yet.
struct GroupChatView: View {
    let group: Group
    let identity: Identity
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @ObservedObject private var connectivityClock = ConnectivityClock.shared

    @Environment(\.dismiss) private var dismiss
    @State private var messages: [StoredMessage] = []
    @State private var contactsById: [Data: Contact] = [:]
    @State private var draft = ""
    @State private var showDetails = false
    @State private var confirmDelete = false
    @State private var cancellable: AnyCancellable?
    @State private var replyingTo: StoredMessage?
    @FocusState private var composerFocused: Bool
    @State private var replyMetadata: [String: MessageReplyMetadata] = [:]
    @State private var photoItem: PhotosPickerItem?
    @State private var showCamera = false
    @State private var pendingPhoto: Data?
    @State private var showVoice = false
    @State private var voiceRecorder = VoiceRecorder()
    @State private var voiceRecording = false
    @State private var statusMessage: String?
    @State private var viewedPhoto: ViewedPhoto?
    @State private var isMuted = false
    @State private var updatedGroup: Group?

    private let store = AppStore.get()
    private var sender: GroupSender { GroupSender(store: store, identity: identity) }
    private var activeGroup: Group { updatedGroup ?? group }

    private var reachableMemberSummary: String {
        let others = activeGroup.memberUserIds.filter { $0 != identity.userId }
        let count = others.count {
            let level = connectivity.level(for: $0, nowMs: connectivityClock.nowMs)
            return level == .nearby || level == .onlineRelay
        }
        return "\(count) of \(others.count) reachable"
    }

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
                                let messageSenderName = senderName(message.senderUserId)
                                let messageColor = ChatListLogic.avatarHueAndInitials(
                                    userId: message.senderUserId,
                                    name: messageSenderName,
                                    displayId: formatUserId(userId: message.senderUserId)
                                ).0
                                let replyKey = replyMessageKey(message)
                                let reactionKey = MessageTarget(
                                    senderUserId: message.senderUserId,
                                    lamport: message.lamport,
                                    kind: message.kind
                                ).stableKey
                                GroupMessageRow(
                                    message: message,
                                    isOwn: message.senderUserId == identity.userId,
                                    groupName: activeGroup.name,
                                    senderLabel: senderLabel(at: index),
                                    contactColor: messageColor,
                                    quoted: replyMetadata[replyKey]?.quoted,
                                    canReply: replyMetadata[replyKey]?.msgId != nil,
                                    reactions: reactions[reactionKey] ?? [],
                                    onReact: { emoji in
                                        sendReaction(to: message, emoji: emoji)
                                    },
                                    onReply: {
                                        replyingTo = message
                                        composerFocused = true
                                    },
                                    onPhotoTap: { jpeg in
                                        viewedPhoto = ViewedPhoto(jpeg: jpeg)
                                    },
                                    onQuotedTap: { target in
                                        withAnimation {
                                            proxy.scrollTo(messageId(target), anchor: .center)
                                        }
                                    }
                                )
                                .swipeToReply {
                                    replyingTo = message
                                    composerFocused = true
                                }
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
                if let pendingPhoto {
                    PendingPhotoPreview(jpeg: pendingPhoto) { self.pendingPhoto = nil }
                }
                HStack(alignment: .bottom, spacing: 8) {
                    Menu {
                        PhotosPicker(selection: $photoItem, matching: .images) {
                            Label("Photo library", systemImage: "photo")
                        }
                        Button { showCamera = true } label: {
                            Label("Take photo", systemImage: "camera")
                        }
                        Button { showVoice = true } label: {
                            Label("Voice memo", systemImage: "mic")
                        }
                    } label: {
                        Image(systemName: "plus.circle.fill").font(.system(size: 28))
                    }
                    .accessibilityLabel("Attach")

                    TextField("Message", text: $draft, axis: .vertical)
                        .lineLimit(1...4)
                        .focused($composerFocused)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .background(
                            Capsule(style: .continuous)
                                .fill(Color(uiColor: .secondarySystemBackground))
                        )

                    if canSend {
                        Button {
                            sendCurrentDraft()
                        } label: {
                            Image(systemName: "arrow.up.circle.fill")
                                .font(.system(size: 32, weight: .semibold))
                        }
                        .accessibilityLabel("Send")
                    } else {
                        HoldToRecordButton(
                            recorder: voiceRecorder,
                            onFinished: sendVoice,
                            onError: { statusMessage = $0 },
                            onAccessibilityFallback: { showVoice = true }
                        )
                    }
                }
            }
            .padding(12)
            .background(.bar)
        }
        .navigationTitle(activeGroup.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Button { showDetails = true } label: {
                    HStack {
                        AvatarView(userId: activeGroup.id, name: activeGroup.name, size: 32, isGroup: true)
                        VStack(alignment: .leading) {
                            Text(activeGroup.name)
                                .font(.headline)
                            Text("\(reachableMemberSummary) · tap for details")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .buttonStyle(.plain)
            }
        }
        .onAppear {
            updatedGroup = (try? store.getGroup(groupId: group.id)) ?? group
            draft = DraftStore.load(chatId: activeGroup.id)
            isMuted = ChatMuteStore.isMuted(activeGroup.id)
            ChatVisibility.setVisible(activeGroup.id)
            MeshController.shared.notifyChatViewed(chatId: activeGroup.id)
            loadContacts()
            reload()
            cancellable = ChatEvents.subject.sink { chatId in
                if chatId == activeGroup.id { reload() }
            }
        }
        .onDisappear {
            ChatVisibility.clearVisible(activeGroup.id)
            ChatEvents.notifyChatChanged(activeGroup.id)
            voiceRecorder.cancel()
        }
        .onChange(of: photoItem) { item in
            guard let item else { return }
            Task {
                if let data = try? await item.loadTransferable(type: Data.self),
                   let jpeg = MediaCompressor.compressImage(data: data) {
                    pendingPhoto = jpeg
                } else {
                    statusMessage = "Could not prepare photo"
                }
                photoItem = nil
            }
        }
        .onChange(of: draft) { DraftStore.save(chatId: activeGroup.id, text: $0) }
        .sheet(isPresented: $showCamera) {
            CameraPicker { image in
                if let jpeg = MediaCompressor.compress(image: image) {
                    pendingPhoto = jpeg
                } else {
                    statusMessage = "Could not prepare photo"
                }
            }
        }
        .sheet(isPresented: $showVoice, onDismiss: {
            voiceRecorder.cancel()
            voiceRecording = false
        }) {
            NavigationStack {
                VStack(spacing: 24) {
                    Image(systemName: voiceRecording ? "waveform.circle.fill" : "mic.circle")
                        .font(.system(size: 72))
                        .foregroundStyle(voiceRecording ? Color.red : Color.accentColor)
                    Text(voiceRecording ? "Recording…" : "Voice memo")
                        .font(.title2.weight(.semibold))
                    Text("Voice memos stop automatically after \(Int(VoiceRecorder.maxDurationSeconds)) seconds.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    if voiceRecording {
                        Button("Stop and send") {
                            if let (url, duration) = voiceRecorder.stop() {
                                sendVoice(url: url, durationMs: duration)
                            }
                            voiceRecording = false
                            showVoice = false
                        }
                        .buttonStyle(.borderedProminent)
                    } else {
                        Button("Start recording") {
                            if voiceRecorder.start() { voiceRecording = true }
                            else { statusMessage = "Microphone unavailable"; showVoice = false }
                        }
                        .buttonStyle(.borderedProminent)
                    }
                    Spacer()
                }
                .padding(24)
                .navigationTitle("Voice memo")
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { showVoice = false }
                    }
                }
            }
        }
        .sheet(isPresented: $showDetails) {
            GroupDetailsSheet(
                group: activeGroup,
                identity: identity,
                contacts: Array(contactsById.values),
                connectivity: connectivity,
                nowMs: connectivityClock.nowMs,
                nameForMember: senderName,
                isMuted: isMuted,
                onMutedChange: {
                    isMuted = $0
                    ChatMuteStore.setMuted($0, chatId: activeGroup.id)
                    ChatEvents.notifyChatChanged(activeGroup.id)
                },
                onRename: { name in
                    guard let changed = sender.renameGroup(group: activeGroup, name: name) else { return false }
                    updatedGroup = changed
                    return true
                },
                onAddMembers: { contacts in
                    guard let changed = sender.addMembers(group: activeGroup, additions: contacts) else { return false }
                    updatedGroup = changed
                    return true
                }
            ) {
                showDetails = false
                confirmDelete = true
            }
        }
        .fullScreenCover(item: $viewedPhoto) { photo in
            PhotoViewerOverlay(jpeg: photo.jpeg)
        }
        .alert("Notice", isPresented: Binding(
            get: { statusMessage != nil },
            set: { if !$0 { statusMessage = nil } }
        )) {
            Button("OK", role: .cancel) { statusMessage = nil }
        } message: {
            Text(statusMessage ?? "")
        }
        .alert("Delete \(activeGroup.name)?", isPresented: $confirmDelete) {
            Button("Delete", role: .destructive) {
                _ = try? store.deleteGroup(groupId: activeGroup.id)
                dismiss()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Removes this group and its message history from this device. Other members keep their copy.")
        }
    }

    private var canSend: Bool {
        pendingPhoto != nil || !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func sendCurrentDraft() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let replyToMsgId = replyingTo.flatMap { replyMetadata[replyMessageKey($0)]?.msgId }
        if let photo = pendingPhoto {
            sender.sendAttachment(
                group: activeGroup,
                attachment: AttachmentPayload(
                    mediaType: .image,
                    mimeType: "image/jpeg",
                    durationMs: 0,
                    blob: photo,
                    caption: text
                ),
                replyToMsgId: replyToMsgId
            )
            pendingPhoto = nil
        } else {
            guard !text.isEmpty else { return }
            sender.sendText(group: activeGroup, text: text, replyToMsgId: replyToMsgId)
        }
        draft = ""
        replyingTo = nil
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        reload()
    }

    private func sendVoice(url: URL, durationMs: Int32) {
        defer { try? FileManager.default.removeItem(at: url) }
        guard let data = try? Data(contentsOf: url), !data.isEmpty else {
            statusMessage = "Could not save voice memo"
            return
        }
        guard data.count <= AttachmentPayload.maxBlobBytes else {
            statusMessage = "Voice memo too large for the mesh"
            return
        }
        sender.sendAttachment(
            group: activeGroup,
            attachment: AttachmentPayload(
                mediaType: .audio,
                mimeType: "audio/mp4",
                durationMs: min(durationMs, 60_000),
                blob: data
            ),
            replyToMsgId: replyingTo.flatMap { replyMetadata[replyMessageKey($0)]?.msgId }
        )
        replyingTo = nil
        reload()
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
        let loadedMessages = (try? store.messagesForChat(chatId: activeGroup.id)) ?? []
        messages = loadedMessages
        replyMetadata = loadMessageReplyMetadata(
            store: store,
            messages: loadedMessages.filter { isVisibleChatKind($0.kind) }
        ) { message in
            senderName(message.senderUserId)
        }
        MeshController.shared.notifyChatViewed(chatId: activeGroup.id)
        if let stored = try? store.getGroup(groupId: activeGroup.id) {
            updatedGroup = stored
        }
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
        sender.sendReaction(group: activeGroup, target: target, emoji: existingOwn ? "" : emoji)
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
    let onPhotoTap: (Data) -> Void
    let onQuotedTap: (StoredMessage) -> Void
    @State private var showInfo = false

    var body: some View {
        let outboundExpiry = isOwn
            ? ((try? AppStore.get().outboundMessageExpiry(
                chatId: message.chatId,
                senderUserId: message.senderUserId,
                lamport: message.lamport
            )) ?? nil)
            : nil

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
                        if message.kind == ProtocolKind.attachmentManifest {
                            if let attachment = AttachmentPayload.decode(message.payload) {
                                switch attachment.mediaType {
                                case .image:
                                    ChatImageView(
                                        jpeg: attachment.blob,
                                        canReply: canReply,
                                        onReply: onReply,
                                        onOpen: onPhotoTap,
                                        onStatus: { _ in }
                                    )
                                case .audio:
                                    VoiceMemoPlayerView(
                                        blob: attachment.blob,
                                        durationMs: attachment.durationMs
                                    )
                                }
                                if !attachment.caption.isEmpty { Text(attachment.caption) }
                            } else {
                                Text("Unsupported attachment")
                            }
                        } else {
                            Text(String(data: message.payload, encoding: .utf8) ?? "")
                        }
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
                            Button {
                                showInfo = true
                            } label: {
                                Label("Info", systemImage: "info.circle")
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
            .sheet(isPresented: $showInfo) {
                MessageInfoSheet(text: messageInfoText(
                    message: message,
                    isOwn: isOwn,
                    tick: nil,
                    arrival: try? AppStore.get().messageArrival(
                        chatId: message.chatId,
                        senderUserId: message.senderUserId,
                        lamport: message.lamport
                    ),
                    outboundExpiryMs: outboundExpiry
                ))
            }
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
    let identity: Identity
    let contacts: [Contact]
    @ObservedObject var connectivity: MeshConnectivityStatus
    let nowMs: Int64
    let nameForMember: (Data) -> String
    let isMuted: Bool
    let onMutedChange: (Bool) -> Void
    let onRename: (String) -> Bool
    let onAddMembers: ([Contact]) -> Bool
    let onDelete: () -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var showRename = false
    @State private var renameDraft = ""
    @State private var showAddMembers = false
    @State private var selectedMemberIds = Set<Data>()
    @State private var actionError: String?

    private var availableContacts: [Contact] {
        contacts
            .filter { !group.memberUserIds.contains($0.userId) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    var body: some View {
        NavigationStack {
            List {
                Section("Group") {
                    LabeledContent("Name", value: group.name)
                    LabeledContent("Members", value: "\(group.memberUserIds.count)")
                    Toggle("Mute notifications", isOn: Binding(
                        get: { isMuted },
                        set: onMutedChange
                    ))
                    Button("Rename group") {
                        renameDraft = group.name
                        actionError = nil
                        showRename = true
                    }
                    Button("Add members") {
                        selectedMemberIds = []
                        actionError = nil
                        showAddMembers = true
                    }
                    .disabled(availableContacts.isEmpty)
                }
                Section("Members") {
                    ForEach(group.memberUserIds, id: \.self) { memberId in
                        HStack(spacing: 12) {
                            AvatarView(
                                userId: memberId,
                                name: nameForMember(memberId),
                                size: 36,
                                reachability: memberId == identity.userId
                                    ? nil
                                    : connectivity.level(for: memberId, nowMs: nowMs)
                            )
                            Text(memberId == identity.userId ? "You" : nameForMember(memberId))
                        }
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
        .sheet(isPresented: $showRename) {
            NavigationStack {
                Form {
                    TextField("Group name", text: $renameDraft)
                    if let actionError {
                        Text(actionError).foregroundStyle(.red)
                    }
                }
                .navigationTitle("Rename group")
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { showRename = false }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Rename") {
                            if onRename(renameDraft) {
                                showRename = false
                            } else {
                                actionError = "Couldn't rename the group. The change was not queued."
                            }
                        }
                        .disabled(
                            renameDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                || renameDraft.trimmingCharacters(in: .whitespacesAndNewlines) == group.name
                        )
                    }
                }
            }
        }
        .sheet(isPresented: $showAddMembers) {
            NavigationStack {
                List(availableContacts, id: \.userId) { contact in
                    Button {
                        if selectedMemberIds.contains(contact.userId) {
                            selectedMemberIds.remove(contact.userId)
                        } else {
                            selectedMemberIds.insert(contact.userId)
                        }
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: selectedMemberIds.contains(contact.userId)
                                ? "checkmark.circle.fill" : "circle")
                            AvatarView(
                                userId: contact.userId,
                                name: contact.name,
                                size: 36,
                                reachability: connectivity.level(for: contact.userId, nowMs: nowMs)
                            )
                            Text(ChatListLogic.displayNameOrId(
                                name: contact.name,
                                displayId: formatUserId(userId: contact.userId)
                            ))
                        }
                    }
                    .buttonStyle(.plain)
                }
                .overlay(alignment: .bottom) {
                    if let actionError {
                        Text(actionError)
                            .foregroundStyle(.red)
                            .padding()
                            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
                    }
                }
                .navigationTitle("Add members")
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { showAddMembers = false }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Add (\(selectedMemberIds.count))") {
                            let additions = availableContacts.filter { selectedMemberIds.contains($0.userId) }
                            if onAddMembers(additions) {
                                showAddMembers = false
                            } else {
                                actionError = "Couldn't add members. No invitations were queued."
                            }
                        }
                        .disabled(selectedMemberIds.isEmpty)
                    }
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
