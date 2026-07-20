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
    @State private var messages: [StoredMessage] = []
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
    @State private var replyMetadata: [String: MessageReplyMetadata] = [:]
    @State private var viewedPhoto: ViewedPhoto?
    @State private var isMuted = false

    private let store = AppStore.get()
    private var sender: RealMeshSender { RealMeshSender(store: store, identity: identity) }

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

    private var visible: [StoredMessage] {
        messages.filter { isVisibleChatKind($0.kind) }
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
                            ForEach(Array(visible.enumerated()), id: \.element.stableRowId) { index, message in
                                if isNewDay(index) {
                                    Text(dayLabel(message.timestamp))
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
                                    reactions: reactions[MessageTarget(
                                        senderUserId: message.senderUserId,
                                        lamport: message.lamport,
                                        kind: message.kind
                                    ).stableKey] ?? [],
                                    grouping: messageGrouping(at: index),
                                    onStatus: { statusMessage = $0 },
                                    onReact: { emoji in
                                        sendReaction(to: message, emoji: emoji)
                                    },
                                    onReply: {
                                        replyingTo = message
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
                                .id(message.stableRowId)
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
                    PendingPhotoPreview(jpeg: pendingPhoto) {
                        self.pendingPhoto = nil
                    }
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
                        Image(systemName: "plus.circle.fill")
                            .font(.system(size: 28))
                    }
                    .accessibilityLabel("Attach")

                    TextField("Message", text: $draft, axis: .vertical)
                        .lineLimit(1...4)
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
        .navigationTitle(ChatListLogic.displayNameOrId(
            name: contact.name,
            displayId: formatUserId(userId: contact.userId)
        ))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Button { showDetails = true } label: {
                    HStack {
                        AvatarView(
                            userId: contact.userId,
                            name: contact.name,
                            size: 32,
                            photo: avatarData.flatMap { UIImage(data: $0) },
                            reachability: reachability
                        )
                        VStack(alignment: .leading) {
                            Text(ChatListLogic.displayNameOrId(
                                name: contact.name,
                                displayId: formatUserId(userId: contact.userId)
                            ))
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
        .onChange(of: draft) { DraftStore.save(chatId: contact.userId, text: $0) }
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
                        .multilineTextAlignment(.center)
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
                            if voiceRecorder.start() {
                                voiceRecording = true
                            } else {
                                statusMessage = "Microphone unavailable"
                                showVoice = false
                            }
                        }
                        .buttonStyle(.borderedProminent)
                    }
                    Spacer()
                }
                .padding(24)
                .navigationTitle("Voice memo")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { showVoice = false }
                    }
                }
            }
        }
        .sheet(isPresented: $showDetails) {
            ContactDetailsSheet(
                contact: contact,
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
        MeshController.shared.notifyChatViewed(chatId: contact.userId)
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

    private func isNewDay(_ index: Int) -> Bool {
        let cal = Calendar.current
        let current = Date(timeIntervalSince1970: TimeInterval(visible[index].timestamp) / 1000)
        guard index > 0 else { return true }
        let previous = Date(timeIntervalSince1970: TimeInterval(visible[index - 1].timestamp) / 1000)
        return !cal.isDate(current, inSameDayAs: previous)
    }

    private func dayLabel(_ timestampMs: Int64) -> String {
        let f = DateFormatter()
        f.dateFormat = "MMMM d, yyyy"
        f.locale = .current
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1000))
    }

    private func messageGrouping(at index: Int) -> MessageGrouping {
        let current = visible[index]
        let previous = index > 0 ? visible[index - 1] : nil
        let next = index + 1 < visible.count ? visible[index + 1] : nil
        return MessageGrouping(
            joinsPrevious: previous.map { shouldGroup($0, current) } ?? false,
            joinsNext: next.map { shouldGroup(current, $0) } ?? false
        )
    }

    private func shouldGroup(_ first: StoredMessage, _ second: StoredMessage) -> Bool {
        guard first.senderUserId == second.senderUserId else { return false }
        let gap = second.timestamp - first.timestamp
        guard gap >= 0 && gap <= 5 * 60 * 1000 else { return false }
        let cal = Calendar.current
        let a = Date(timeIntervalSince1970: TimeInterval(first.timestamp) / 1000)
        let b = Date(timeIntervalSince1970: TimeInterval(second.timestamp) / 1000)
        return cal.isDate(a, inSameDayAs: b)
    }

    private func scrollToLatest(proxy: ScrollViewProxy, animated: Bool = true) {
        guard let last = visible.last else { return }
        if animated {
            withAnimation { proxy.scrollTo(last.stableRowId, anchor: .bottom) }
        } else {
            proxy.scrollTo(last.stableRowId, anchor: .bottom)
        }
    }
}

private struct MessageGrouping {
    let joinsPrevious: Bool
    let joinsNext: Bool

    var showTimestamp: Bool { !joinsNext }
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
                    Text(timeLabel(message.timestamp))
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
            MessageInfoSheet(text: messageInfoText(
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

    private func timeLabel(_ ms: Int64) -> String {
        let f = DateFormatter()
        f.dateFormat = "h:mm a"
        f.locale = .current
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
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

private struct ReactionActionBar: View {
    let onReact: (String) -> Void

    var body: some View {
        HStack(spacing: 2) {
            ForEach(reactionChoices, id: \.self) { emoji in
                Button {
                    onReact(emoji)
                } label: {
                    Text(emoji)
                        .font(.title2)
                        .frame(width: 38, height: 38)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(
            Capsule(style: .continuous)
                .fill(Color(uiColor: .secondarySystemBackground))
                .shadow(color: .black.opacity(0.18), radius: 10, y: 4)
        )
        .padding(.bottom, 4)
    }
}

private struct MessageActionPanel: View {
    let canCopy: Bool
    let onCopy: () -> Void
    let onInfo: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Button(action: onCopy) {
                Label("Copy", systemImage: "doc.on.doc")
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
            }
            .disabled(!canCopy)

            Divider().padding(.leading, 16)

            Button(action: onInfo) {
                Label("Info", systemImage: "info.circle")
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
            }
        }
        .buttonStyle(.plain)
        .frame(width: 190)
        .foregroundStyle(Color.primary)
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(Color(uiColor: .secondarySystemBackground))
                .shadow(color: .black.opacity(0.18), radius: 10, y: 4)
        )
        .padding(.top, 4)
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

func messageInfoText(
    message: StoredMessage,
    isOwn: Bool,
    tick: TickStatus?,
    arrival: MessageArrival? = nil,
    deliveredViaRoute: String? = nil,
    outboundExpiryMs: Int64? = nil,
    nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
) -> String {
    let f = DateFormatter()
    f.dateFormat = "MMMM d, yyyy h:mm a"
    f.locale = .current
    let sentAt = f.string(from: Date(timeIntervalSince1970: TimeInterval(message.timestamp) / 1000))
    let direction = isOwn ? "Sent by you" : "Received"
    let status: String
    if isOwn, tick == .sent, let expiry = outboundExpiryMs, expiry <= nowMs {
        status = "\nStatus: Not delivered — expired"
    } else if isOwn, tick == .sent, let expiry = outboundExpiryMs {
        status = "\nStatus: Still trying — expires in \(expiryRemainingText(expiry - nowMs))"
    } else {
        status = tick.map { "\nStatus: \(tickLegendText($0))" } ?? ""
    }
    let arrivalLine: String
    if isOwn {
        arrivalLine = deliveredViaRoute.map { "\nDelivery confirmed via \($0)" } ?? ""
    } else {
        arrivalLine = arrival.map { "\n\(messageArrivalText($0))" } ?? ""
    }
    return "\(direction)\nTime: \(sentAt)\(status)\(arrivalLine)"
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
    let text: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List(Array(text.split(separator: "\n").enumerated()), id: \.offset) { entry in
                let line = entry.element
                let parts = line.split(separator: ":", maxSplits: 1).map(String.init)
                if parts.count == 2 {
                    LabeledContent(parts[0], value: parts[1].trimmingCharacters(in: .whitespaces))
                } else {
                    Text(String(line))
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


