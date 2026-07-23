import AVFoundation
import Combine
import PhotosUI
import SwiftUI
import UIKit

struct ChatView: View {
    let contact: Contact
    let identity: Identity
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @ObservedObject private var connectivityClock = ConnectivityClock.shared

    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var messages: [StoredMessage] = []
    @State private var rows: [ChatRowModel] = []
    @State private var avatarData: Data?
    @State private var deliveredThrough: UInt64 = 0
    @State private var readThrough: UInt64 = 0
    @State private var draft = ""
    @State private var showVoice = false
    @State private var showDetails = false
    @State private var confirmDelete = false
    @State private var photoItem: PhotosPickerItem?
    @State private var showCamera = false
    @State private var pendingPhoto: Data?
    @State private var statusMessage: String?
    @State private var cancellable: AnyCancellable?
    @State private var voiceRecorder = VoiceRecorder()
    @State private var voiceRecording = false
    @State private var replyingTo: StoredMessage?
    @FocusState private var composerFocused: Bool
    @State private var replyMetadata: [String: MessageReplyMetadata] = [:]
    @State private var viewedPhoto: ViewedPhoto?
    @State private var isMuted = false
    @State private var isBlocked = false
    @State private var localNickname: String?
    @State private var nicknameEdited = false

    private let store = AppStore.get()
    private var sender: RealMeshSender { RealMeshSender(store: store, identity: identity) }

    /// `contact` with any in-session nickname edit (incl. clearing) applied, so
    /// the header and the open details sheet reflect a change immediately (T16).
    /// `nicknameEdited` lets that win over the value `contact` was built with.
    private var displayContact: Contact {
        var c = contact
        c.nickname = nicknameEdited ? localNickname : contact.nickname
        return c
    }

    /// The name to show in the header/title: the local nickname when set,
    /// otherwise the card name.
    private var resolvedName: String {
        ChatListLogic.displayNameOrId(
            name: coreContactDisplayName(contact: displayContact),
            displayId: formatUserId(userId: contact.userId)
        )
    }

    private var reachability: ReachabilityLevel {
        connectivity.level(for: contact.userId, nowMs: connectivityClock.nowMs)
    }

    private var reachabilityText: String {
        ContactReachability.chatHeaderCopy(
            reachability,
            peerLastSeenMs: connectivity.contactLastSeen[contact.userId],
            nowMs: connectivityClock.nowMs
        )
    }

    private var reactions: [String: [ReactionSummary]] {
        reactionSummariesByTarget(messages: messages, ownUserId: identity.userId)
    }

    private var replyingToPreview: QuotedMessagePreview? {
        replyingTo.map { target in
            quotedMessagePreview(target: target, senderLabelFor: senderLabel)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            // Bottom-anchor the thread so a new/short chat keeps the latest
            // bubble just above the composer (and keyboard), matching Android.
            GeometryReader { geo in
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 4) {
                            ForEach(rows, id: \.rowId) { row in
                                let message = row.message
                                if row.showDayBreak {
                                    Text(row.dayLabel)
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                        .frame(maxWidth: .infinity)
                                        .padding(.vertical, 6)
                                }
                                MessageBubbleView(
                                    message: message,
                                    isOwn: message.senderUserId == identity.userId,
                                    tick: message.senderUserId == identity.userId
                                        ? tickStatusFor(
                                            lamport: message.lamport,
                                            deliveredThrough: deliveredThrough,
                                            readThrough: readThrough
                                        )
                                        : nil,
                                    contactColor: ChatListLogic.avatarHueAndInitials(
                                        userId: contact.userId,
                                        name: contact.name,
                                        displayId: formatUserId(userId: contact.userId)
                                    ).0,
                                    quoted: replyMetadata[replyMessageKey(message)]?.quoted,
                                    canReply: replyMetadata[replyMessageKey(message)]?.msgId != nil,
                                    reactions: row.reactions,
                                    grouping: row.grouping,
                                    timeLabel: row.timeLabel,
                                    onStatus: { statusMessage = $0 },
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
                                            proxy.scrollTo(target.stableRowId, anchor: .center)
                                        }
                                    }
                                )
                                .swipeToReply {
                                    replyingTo = message
                                    composerFocused = true
                                }
                                .id(row.rowId)
                            }
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .frame(minHeight: geo.size.height, alignment: .bottom)
                    }
                    .scrollDismissesKeyboard(.interactively)
                    .onChange(of: rows.count) { _ in
                        scrollToLatest(proxy: proxy)
                    }
                    .onAppear {
                        scrollToLatest(proxy: proxy, animated: false)
                    }
                }
            }

            ChatComposerBar(
                replyingToPreview: replyingToPreview,
                pendingPhoto: pendingPhoto,
                draft: $draft,
                photoItem: $photoItem,
                showCamera: $showCamera,
                showVoice: $showVoice,
                composerFocused: $composerFocused,
                voiceRecorder: voiceRecorder,
                canSend: canSend,
                onCancelReply: { replyingTo = nil },
                onRemovePhoto: { pendingPhoto = nil },
                onSend: sendCurrentDraft,
                onVoiceFinished: sendVoice,
                onVoiceError: { statusMessage = $0 }
            )
        }
        .navigationTitle(resolvedName)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Button { showDetails = true } label: {
                    HStack {
                        AvatarView(
                            userId: contact.userId,
                            name: resolvedName,
                            size: 32,
                            photo: avatarData.flatMap { UIImage(data: $0) },
                            reachability: reachability
                        )
                        VStack(alignment: .leading) {
                            Text(resolvedName)
                            .font(.headline)
                            Text(reachabilityText)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .buttonStyle(.plain)
            }
        }
        .onAppear {
            draft = DraftStore.load(chatId: contact.userId)
            isMuted = ChatMuteStore.isMuted(contact.userId)
            isBlocked = (try? store.isUserBlocked(userId: contact.userId)) ?? false
            ChatVisibility.setVisible(contact.userId)
            MeshController.shared.notifyChatViewed(chatId: contact.userId)
            reload()
            cancellable = ChatEvents.subject.sink { chatId in
                if chatId == contact.userId { reload() }
            }
        }
        .onDisappear {
            ChatVisibility.clearVisible(contact.userId)
            voiceRecorder.cancel()
        }
        .onChange(of: scenePhase) { phase in
            // Backgrounding leaves the view "appeared" (no onDisappear), so a
            // locked phone would otherwise keep this chat marked visible —
            // false read receipts and suppressed notifications while asleep
            // (FI1). Mirrors Android's ON_STOP/ON_START ChatVisibility reset.
            if phase == .background {
                ChatVisibility.clearVisible(contact.userId)
            } else if phase == .active {
                ChatVisibility.setVisible(contact.userId)
                MeshController.shared.notifyChatViewed(chatId: contact.userId)
            }
        }
        .onChange(of: draft) { DraftStore.save(chatId: contact.userId, text: $0) }
        .chatAttachmentPipeline(
            photoItem: $photoItem,
            showCamera: $showCamera,
            showVoice: $showVoice,
            voiceRecording: $voiceRecording,
            voiceRecorder: voiceRecorder,
            onPhotoReady: { pendingPhoto = $0 },
            onAttachmentError: { statusMessage = $0 },
            onVoiceSend: sendVoice
        )
        .sheet(isPresented: $showDetails) {
            ContactDetailsSheet(
                contact: displayContact,
                avatarData: avatarData,
                reachability: reachability,
                connectivityText: ContactReachability.contactDetailsCopy(
                    reachability,
                    peerLastSeenMs: connectivity.contactLastSeen[contact.userId],
                    presenceLastSeenMs: connectivity.presenceLastSeen[contact.userId],
                    nowMs: connectivityClock.nowMs
                ),
                isMuted: isMuted,
                onMutedChange: {
                    isMuted = $0
                    ChatMuteStore.setMuted($0, chatId: contact.userId)
                    ChatEvents.notifyChatChanged(contact.userId)
                },
                onSetNickname: { nickname in
                    _ = try? store.setContactNickname(userId: contact.userId, nickname: nickname)
                    localNickname = nickname
                    nicknameEdited = true
                    ChatEvents.notifyChatChanged(contact.userId)
                },
                isBlocked: isBlocked,
                onBlockedChange: { blocked in
                    if blocked {
                        try? store.blockUser(
                            userId: contact.userId,
                            nowMs: Int64(Date().timeIntervalSince1970 * 1000)
                        )
                    } else {
                        _ = try? store.unblockUser(userId: contact.userId)
                    }
                    isBlocked = blocked
                },
                onReport: {
                    launchContactReport(contact: displayContact, reporterUserId: identity.userId)
                }
            ) {
                showDetails = false
                confirmDelete = true
            }
        }
        .fullScreenCover(item: $viewedPhoto) { photo in
            PhotoViewerOverlay(jpeg: photo.jpeg)
        }
        .alert("Delete contact?", isPresented: $confirmDelete) {
            Button("Delete", role: .destructive) {
                try? store.deleteContact(userId: contact.userId)
                FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
                dismiss()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Removes the contact and chat history.")
        }
        .alert("Notice", isPresented: Binding(
            get: { statusMessage != nil },
            set: { if !$0 { statusMessage = nil } }
        )) {
            Button("OK", role: .cancel) { statusMessage = nil }
        } message: {
            Text(statusMessage ?? "")
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
                contact: contact,
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
            draft = ""
            replyingTo = nil
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            reload()
            return
        }
        guard !text.isEmpty else { return }
        sender.sendText(contact: contact, text: text, replyToMsgId: replyToMsgId)
        draft = ""
        replyingTo = nil
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        reload()
    }

    private func reload() {
        let loadedMessages = (try? store.messagesForChat(chatId: contact.userId)) ?? []
        messages = loadedMessages
        rows = ChatRowModel.build(from: loadedMessages, ownUserId: identity.userId)
        replyMetadata = loadMessageReplyMetadata(
            store: store,
            messages: loadedMessages.filter { isVisibleChatKind($0.kind) },
            senderLabelFor: senderLabel
        )
        avatarData = (try? store.contactAvatar(userId: contact.userId)) ?? nil
        deliveredThrough = (try? store.receiptThrough(
            chatId: contact.userId,
            senderUserId: identity.userId,
            receiptType: ReceiptType.delivered
        )) ?? 0
        readThrough = (try? store.receiptThrough(
            chatId: contact.userId,
            senderUserId: identity.userId,
            receiptType: ReceiptType.read
        )) ?? 0
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
            contact: contact,
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

    private func sendReaction(to message: StoredMessage, emoji: String) {
        let target = MessageTarget(
            senderUserId: message.senderUserId,
            lamport: message.lamport,
            kind: message.kind
        )
        let existingOwn = reactions[target.stableKey]?.contains {
            $0.emoji == emoji && $0.reactedByOwnUser
        } ?? false
        sender.sendReaction(contact: contact, target: target, emoji: existingOwn ? "" : emoji)
        reload()
    }

    private func senderLabel(_ message: StoredMessage) -> String {
        if message.senderUserId == identity.userId { return "You" }
        return ChatListLogic.displayNameOrId(
            name: contact.name,
            displayId: formatUserId(userId: contact.userId)
        )
    }

    private func scrollToLatest(proxy: ScrollViewProxy, animated: Bool = true) {
        guard let last = rows.last else { return }
        if animated {
            withAnimation { proxy.scrollTo(last.rowId, anchor: .bottom) }
        } else {
            proxy.scrollTo(last.rowId, anchor: .bottom)
        }
    }
}

struct MessageGrouping: Equatable {
    let joinsPrevious: Bool
    let joinsNext: Bool

    var showTimestamp: Bool { !joinsNext }
}

/// A single row of a 1:1 chat thread, precomputed once per `reload()` (FI8):
/// the day-break flag/label, message grouping, and reaction summary used to
/// be recomputed per row per SwiftUI body pass (each recomputation itself
/// O(n) over the message list), making the whole body O(n^2) for an
/// n-message thread. `build(from:ownUserId:)` computes all of it in one O(n)
/// pass over the (already-loaded) messages so the view body just reads
/// precomputed fields.
struct ChatRowModel: Equatable {
    let message: StoredMessage
    let rowId: String
    let showDayBreak: Bool
    let dayLabel: String
    let grouping: MessageGrouping
    let timeLabel: String
    let reactions: [ReactionSummary]

    private static let dayFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "MMMM d, yyyy"
        f.locale = .current
        return f
    }()

    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "h:mm a"
        f.locale = .current
        return f
    }()

    static func build(from messages: [StoredMessage], ownUserId: Data) -> [ChatRowModel] {
        let visible = messages.filter { isVisibleChatKind($0.kind) }
        let reactionsByTarget = reactionSummariesByTarget(messages: messages, ownUserId: ownUserId)
        let cal = Calendar.current
        var rows: [ChatRowModel] = []
        rows.reserveCapacity(visible.count)
        for (index, message) in visible.enumerated() {
            let date = Date(timeIntervalSince1970: TimeInterval(message.timestamp) / 1000)
            let showDayBreak: Bool
            if index == 0 {
                showDayBreak = true
            } else {
                let previousDate = Date(timeIntervalSince1970: TimeInterval(visible[index - 1].timestamp) / 1000)
                showDayBreak = !cal.isDate(date, inSameDayAs: previousDate)
            }
            let joinsPrevious = index > 0 && shouldGroup(visible[index - 1], message)
            let joinsNext = index + 1 < visible.count && shouldGroup(message, visible[index + 1])
            let target = MessageTarget(senderUserId: message.senderUserId, lamport: message.lamport, kind: message.kind)
            rows.append(ChatRowModel(
                message: message,
                rowId: message.stableRowId,
                showDayBreak: showDayBreak,
                dayLabel: showDayBreak ? dayFormatter.string(from: date) : "",
                grouping: MessageGrouping(joinsPrevious: joinsPrevious, joinsNext: joinsNext),
                timeLabel: timeFormatter.string(from: date),
                reactions: reactionsByTarget[target.stableKey] ?? []
            ))
        }
        return rows
    }

    private static func shouldGroup(_ first: StoredMessage, _ second: StoredMessage) -> Bool {
        guard first.senderUserId == second.senderUserId else { return false }
        let gap = second.timestamp - first.timestamp
        guard gap >= 0 && gap <= 5 * 60 * 1000 else { return false }
        let cal = Calendar.current
        let a = Date(timeIntervalSince1970: TimeInterval(first.timestamp) / 1000)
        let b = Date(timeIntervalSince1970: TimeInterval(second.timestamp) / 1000)
        return cal.isDate(a, inSameDayAs: b)
    }
}

private struct ChatBubbleShape: Shape {
    let topLeadingRadius: CGFloat
    let bottomLeadingRadius: CGFloat
    let bottomTrailingRadius: CGFloat
    let topTrailingRadius: CGFloat

    func path(in rect: CGRect) -> Path {
        let maxRadius = min(rect.width, rect.height) / 2
        let tl = min(topLeadingRadius, maxRadius)
        let tr = min(topTrailingRadius, maxRadius)
        let br = min(bottomTrailingRadius, maxRadius)
        let bl = min(bottomLeadingRadius, maxRadius)
        var path = Path()
        path.move(to: CGPoint(x: rect.minX + tl, y: rect.minY))
        path.addLine(to: CGPoint(x: rect.maxX - tr, y: rect.minY))
        path.addQuadCurve(to: CGPoint(x: rect.maxX, y: rect.minY + tr), control: CGPoint(x: rect.maxX, y: rect.minY))
        path.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY - br))
        path.addQuadCurve(to: CGPoint(x: rect.maxX - br, y: rect.maxY), control: CGPoint(x: rect.maxX, y: rect.maxY))
        path.addLine(to: CGPoint(x: rect.minX + bl, y: rect.maxY))
        path.addQuadCurve(to: CGPoint(x: rect.minX, y: rect.maxY - bl), control: CGPoint(x: rect.minX, y: rect.maxY))
        path.addLine(to: CGPoint(x: rect.minX, y: rect.minY + tl))
        path.addQuadCurve(to: CGPoint(x: rect.minX + tl, y: rect.minY), control: CGPoint(x: rect.minX, y: rect.minY))
        path.closeSubpath()
        return path
    }
}

private struct MessageBubbleView: View {
    let message: StoredMessage
    let isOwn: Bool
    let tick: TickStatus?
    let contactColor: Color
    let quoted: QuotedMessagePreview?
    let canReply: Bool
    let reactions: [ReactionSummary]
    let grouping: MessageGrouping
    let timeLabel: String
    var onStatus: (String) -> Void = { _ in }
    var onReact: (String) -> Void = { _ in }
    var onReply: () -> Void = {}
    var onPhotoTap: (Data) -> Void = { _ in }
    var onQuotedTap: (StoredMessage) -> Void = { _ in }
    @State private var showLegend = false
    @State private var showInfo = false

    var body: some View {
        let outboundExpiry = isOwn
            ? ((try? AppStore.get().outboundMessageExpiry(
                chatId: message.chatId,
                senderUserId: message.senderUserId,
                lamport: message.lamport
            )) ?? nil)
            : nil

        HStack {
            if isOwn { Spacer(minLength: 40) }
            VStack(alignment: isOwn ? .trailing : .leading, spacing: 4) {
                VStack(alignment: .leading, spacing: 6) {
                    if let quoted {
                        QuotedMessageBlock(
                            preview: quoted,
                            accentColor: isOwn ? .white : .accentColor,
                            contentColor: isOwn ? .white : .primary,
                            onTap: quoted.target.map { target in
                                { onQuotedTap(target) }
                            }
                        )
                    }
                    content
                    if let tick {
                        HStack {
                            Spacer(minLength: 0)
                            SignalTickView(status: tick, tint: isOwn ? .white : .secondary)
                        }
                    }
                }
                .padding(10)
                .background(
                    bubbleShape
                        .fill(isOwn ? Color.accentColor : contactColor.opacity(0.24))
                )
                .foregroundStyle(isOwn ? Color.white : Color.primary)
                .onTapGesture {
                    if tick != nil {
                        showLegend = true
                    }
                }
                .contextMenu {
                    if canReply {
                        Button(action: onReply) {
                            Label("Reply", systemImage: "arrowshape.turn.up.left")
                        }
                    }
                    ForEach(reactionChoices, id: \.self) { emoji in
                        Button(emoji) {
                            UIImpactFeedbackGenerator(style: .light).impactOccurred()
                            onReact(emoji)
                        }
                    }
                    if !messageCopyText(message).isEmpty {
                        Button {
                            UIPasteboard.general.string = messageCopyText(message)
                            onStatus("Copied")
                        } label: {
                            Label("Copy", systemImage: "doc.on.doc")
                        }
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

                if tick == .sent,
                   let expiry = outboundExpiry,
                   expiry <= Int64(Date().timeIntervalSince1970 * 1_000) {
                    Text("Not delivered")
                        .font(.caption2)
                        .foregroundStyle(.red)
                }

                if grouping.showTimestamp {
                    Text(timeLabel)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            if !isOwn { Spacer(minLength: 40) }
        }
        .padding(.top, grouping.joinsPrevious ? 1 : 8)
        .padding(.bottom, grouping.joinsNext ? 1 : 4)
        .overlay(alignment: .bottom) {
            if showLegend, let tick {
                Text(tickLegendText(tick))
                    .font(.caption)
                    .padding(12)
                    .background(.regularMaterial, in: Capsule())
                    .shadow(radius: 4)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .task {
                        try? await Task.sleep(nanoseconds: 2_000_000_000)
                        withAnimation { showLegend = false }
                    }
            }
        }
        .animation(.easeInOut(duration: 0.2), value: showLegend)
        .sheet(isPresented: $showInfo) {
            MessageInfoSheet(rows: messageInfoRows(
                message: message,
                isOwn: isOwn,
                tick: tick,
                arrival: isOwn ? nil : (try? AppStore.get().messageArrival(
                    chatId: message.chatId,
                    senderUserId: message.senderUserId,
                    lamport: message.lamport
                )),
                deliveredViaRoute: isOwn ? deliveryConfirmationRoute(for: message) : nil,
                outboundExpiryMs: outboundExpiry
            ))
        }
    }

    @ViewBuilder
    private var content: some View {
        if message.kind == ProtocolKind.attachmentManifest {
            if let attachment = AttachmentPayload.decode(message.payload) {
                switch attachment.mediaType {
                case .image:
                    ChatImageView(
                        jpeg: attachment.blob,
                        canReply: canReply,
                        onReply: onReply,
                        onOpen: onPhotoTap,
                        onStatus: onStatus
                    )
                case .audio:
                    VoiceMemoPlayerView(blob: attachment.blob, durationMs: attachment.durationMs)
                }
                if !attachment.caption.isEmpty {
                    Text(attachment.caption)
                }
            } else {
                Text("Unsupported attachment")
            }
        } else {
            Text(String(data: message.payload, encoding: .utf8) ?? "")
        }
    }

    private var bubbleShape: ChatBubbleShape {
        ChatBubbleShape(
            topLeadingRadius: !isOwn && grouping.joinsPrevious ? 6 : 18,
            bottomLeadingRadius: !isOwn && grouping.joinsNext ? 6 : 18,
            bottomTrailingRadius: isOwn && grouping.joinsNext ? 6 : 18,
            topTrailingRadius: isOwn && grouping.joinsPrevious ? 6 : 18
        )
    }
}

struct PendingPhotoPreview: View {
    let jpeg: Data
    let onRemove: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            if let image = UIImage(data: jpeg) {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFill()
                    .frame(width: 72, height: 72)
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("Photo ready")
                    .font(.subheadline.weight(.semibold))
                Text("Add a caption or send as-is.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button(action: onRemove) {
                Image(systemName: "xmark.circle.fill")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }
            .accessibilityLabel("Remove photo")
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(Color(uiColor: .secondarySystemBackground))
        )
    }

}

private func messageCopyText(_ message: StoredMessage) -> String {
    if message.kind == ProtocolKind.attachmentManifest {
        return AttachmentPayload.decode(message.payload)?.caption ?? ""
    }
    return String(data: message.payload, encoding: .utf8) ?? ""
}

/// The transport an own message's delivery receipt returned on (T6), resolved
/// from the delivery watermark so it shows for every acknowledged message, not
/// just the one at the exact watermark lamport. `nil` when the message isn't
/// delivered yet or the return route wasn't recorded.
func deliveryConfirmationRoute(for message: StoredMessage) -> String? {
    let store = AppStore.get()
    guard
        let through = try? store.receiptThrough(
            chatId: message.chatId,
            senderUserId: message.senderUserId,
            receiptType: ReceiptType.delivered
        ),
        message.lamport <= through,
        let via = (try? store.receiptViaTransport(
            chatId: message.chatId,
            senderUserId: message.senderUserId,
            receiptType: ReceiptType.delivered
        )) ?? nil
    else { return nil }
    return transportRouteText(via)
}

/// A single row of the Message-info sheet: either a labeled field (rendered
/// as `LabeledContent`) or a free-standing sentence (rendered as plain
/// `Text`). Replaces building one big string and splitting each line on its
/// first `:` to guess which rows had a label -- which corrupted any
/// sentence that happened to contain a colon of its own, e.g. "Arrived via
/// BLE · ~2 hops · 5:14 PM" split into "…· 5" / "14 PM".
enum MessageInfoRow: Equatable {
    case labeled(label: String, value: String)
    case sentence(String)
}

func messageInfoRows(
    message: StoredMessage,
    isOwn: Bool,
    tick: TickStatus?,
    arrival: MessageArrival? = nil,
    deliveredViaRoute: String? = nil,
    outboundExpiryMs: Int64? = nil,
    nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
) -> [MessageInfoRow] {
    let f = DateFormatter()
    f.dateFormat = "MMMM d, yyyy h:mm a"
    f.locale = .current
    let sentAt = f.string(from: Date(timeIntervalSince1970: TimeInterval(message.timestamp) / 1000))

    var rows: [MessageInfoRow] = [
        .sentence(isOwn ? "Sent by you" : "Received"),
        .labeled(label: "Time", value: sentAt),
    ]

    if isOwn, tick == .sent, let expiry = outboundExpiryMs, expiry <= nowMs {
        rows.append(.labeled(label: "Status", value: "Not delivered — expired"))
    } else if isOwn, tick == .sent, let expiry = outboundExpiryMs {
        rows.append(.labeled(
            label: "Status",
            value: "Still trying — expires in \(expiryRemainingText(expiry - nowMs))"
        ))
    } else if let tick {
        rows.append(.labeled(label: "Status", value: tickLegendText(tick)))
    }

    if isOwn {
        if let deliveredViaRoute {
            rows.append(.sentence("Delivery confirmed via \(deliveredViaRoute)"))
        }
    } else if let arrival {
        rows.append(.sentence(messageArrivalText(arrival)))
    }

    return rows
}

private func expiryRemainingText(_ remainingMs: Int64) -> String {
    let minutes = (max(0, remainingMs) + 59_999) / 60_000
    if minutes >= 2 * 24 * 60 { return "\((minutes + 1_439) / 1_440) days" }
    if minutes >= 24 * 60 { return "1 day" }
    if minutes >= 120 { return "\((minutes + 59) / 60) hours" }
    if minutes >= 60 { return "1 hour" }
    return "\(minutes) minutes"
}

func transportRouteText(_ transport: UInt8) -> String {
    switch transport {
    case 0: return "direct BLE"
    case 1: return "another device over BLE"
    case 2: return "relay"
    case 3: return "local Wi-Fi"
    case 4: return "another device over local Wi-Fi"
    default: return "unknown route"
    }
}

private func messageRouteText(_ arrival: MessageArrival) -> String {
    transportRouteText(arrival.transport)
}

private func messageArrivalText(_ arrival: MessageArrival) -> String {
    let hops = Int(arrival.hopsTaken)
    let hopLabel = "~\(hops) \(hops == 1 ? "hop" : "hops")"
    return "Arrived via \(messageRouteText(arrival)) · \(hopLabel) · \(arrivalTime(arrival.receivedAt))"
}

private func arrivalTime(_ timestampMs: Int64) -> String {
    let formatter = DateFormatter()
    formatter.dateFormat = "h:mm a"
    formatter.locale = .current
    return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1_000))
}

private extension StoredMessage {
    var stableRowId: String {
        let sender = senderUserId.map { String(format: "%02x", $0) }.joined()
        return "\(sender):\(lamport):\(kind)"
    }
}

struct ViewedPhoto: Identifiable {
    let id = UUID()
    let jpeg: Data
}

struct MessageInfoSheet: View {
    let rows: [MessageInfoRow]
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List(Array(rows.enumerated()), id: \.offset) { _, row in
                switch row {
                case .labeled(let label, let value):
                    LabeledContent(label, value: value)
                case .sentence(let text):
                    Text(text)
                }
            }
            .navigationTitle("Message info")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium])
    }
}

/// Chat photo: keeps native aspect ratio and offers Save via long-press.
struct ChatImageView: View {
    let jpeg: Data
    var canReply = false
    var onReply: () -> Void = {}
    var onOpen: (Data) -> Void = { _ in }
    var onStatus: (String) -> Void = { _ in }

    var body: some View {
        if let ui = UIImage(data: jpeg) {
            Image(uiImage: ui)
                .resizable()
                .scaledToFit()
                .frame(maxWidth: 280, maxHeight: 360)
                .clipShape(RoundedRectangle(cornerRadius: 12))
                .contentShape(Rectangle())
                .onTapGesture {
                    onOpen(jpeg)
                }
                .contextMenu {
                    if canReply {
                        Button(action: onReply) {
                            Label("Reply", systemImage: "arrowshape.turn.up.left")
                        }
                    }
                    Button {
                        ImageGallery.saveJpeg(jpeg) { result in
                            switch result {
                            case .saved:
                                onStatus("Saved to Photos")
                            case .denied:
                                onStatus("Photo Library access is required to save images. Enable it in Settings.")
                            case .failed(let message):
                                onStatus(message)
                            }
                        }
                    } label: {
                        Label("Save image", systemImage: "square.and.arrow.down")
                    }
                }
                .accessibilityHint("Double-tap to view full screen; long-press for message options")
        } else {
            Text("Photo (could not display)")
        }
    }
}

struct VoiceMemoPlayerView: View {
    let blob: Data
    let durationMs: Int32
    @State private var player: AVAudioPlayer?
    @State private var playing = false

    var body: some View {
        HStack {
            Button {
                if playing {
                    player?.stop()
                    playing = false
                } else {
                    do {
                        let p = try AVAudioPlayer(data: blob)
                        p.play()
                        player = p
                        playing = true
                        DispatchQueue.main.asyncAfter(deadline: .now() + p.duration) {
                            if player === p {
                                player = nil
                                playing = false
                            }
                        }
                    } catch {
                        playing = false
                    }
                }
            } label: {
                Image(systemName: playing ? "stop.fill" : "play.fill")
            }
            let secs = max(1, Int((durationMs + 500) / 1000))
            Text("Voice memo · \(secs / 60):\(String(format: "%02d", secs % 60))")
                .font(.subheadline)
        }
    }
}

struct CameraPicker: UIViewControllerRepresentable {
    var onImage: (UIImage) -> Void
    @Environment(\.dismiss) private var dismiss

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = UIImagePickerController.isSourceTypeAvailable(.camera) ? .camera : .photoLibrary
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ uiViewController: UIImagePickerController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, UINavigationControllerDelegate, UIImagePickerControllerDelegate {
        let parent: CameraPicker
        init(_ parent: CameraPicker) { self.parent = parent }

        func imagePickerController(
            _ picker: UIImagePickerController,
            didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
        ) {
            if let image = info[.originalImage] as? UIImage {
                parent.onImage(image)
            }
            parent.dismiss()
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            parent.dismiss()
        }
    }
}


