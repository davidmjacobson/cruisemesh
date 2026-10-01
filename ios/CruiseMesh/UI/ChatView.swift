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
    @State private var previousScrollRowIds: [String] = []
    @State private var isNearConversationBottom = true
    @State private var showNewMessages = false
    @State private var newMessagesTargetRowId: String?
    @State private var avatarData: Data?
    @State private var deliveredThrough: UInt64 = 0
    @State private var readThrough: UInt64 = 0
    @State private var draft = ""
    @State private var showVoice = false
    @State private var showDetails = false
    @State private var shareContact = false
    @State private var confirmDelete = false
    @State private var photoItem: PhotosPickerItem?
    @State private var showCamera = false
    @State private var pendingPhoto: Data?
    /// The staged photo currently open in the markup editor, or nil when it is
    /// closed (`specs/photo-markup.md`).
    @State private var drawingPhoto: DrawingPhoto?
    @State private var statusMessage: String?
    @State private var cancellable: AnyCancellable?
    @State private var voiceRecorder = VoiceRecorder()
    @State private var voiceRecording = false
    /// Playback for every voice message in this chat, held above the list so a
    /// message survives its bubble scrolling out of view. `@State`, not
    /// `@StateObject`: this screen holds the player but must not redraw the
    /// whole thread ten times a second while a message plays — the voice
    /// bubble is the only view that observes it.
    @State private var voicePlayback = VoiceMemoPlaybackController()
    @State private var replyingTo: StoredMessage?
    @FocusState private var composerFocused: Bool
    @State private var replyMetadata: [String: MessageReplyMetadata] = [:]
    @State private var viewedPhoto: ViewedPhoto?
    @State private var isMuted = false
    @State private var isBlocked = false
    @State private var localNickname: String?
    @State private var nicknameEdited = false
    /// `ContactProvenance.addedNearby`: were we standing next to this person
    /// when we accepted them? A durable fact, so it is read once per chat
    /// rather than on every connectivity tick.
    @State private var addedNearby = false
    @State private var identityCloneWarning = false
    /// §10.4: this contact's devices changed under them and nobody has told them
    /// so yet. Read here rather than on a settings screen, for exactly the reason
    /// `IdentityCloneNotice` gives — the moment that matters is the moment before
    /// somebody types.
    @State private var safetyFact: ContactSafetyFact?

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

    /// Which direction of this chat cannot cross the internet. Local knowledge
    /// only -- our own config, their card, whether a link exists right now, and
    /// whether we ever stood next to them -- so it costs no round trip and is
    /// right even with no connectivity at all.
    private var composerReachVerdict: ComposerReach {
        let own = RelayConfigStore.load()
        return composerReach(
            delivery: contactDelivery(
                contactRelayUrl: contact.relayUrl,
                contactRelayToken: contact.relayToken,
                ownRelayUrl: own?.relayUrl,
                ownRelayToken: own?.relayToken
            ),
            ownRelayConfigured: own != nil,
            contactNearby: connectivity.nearbyPeerIds.contains(contact.userId),
            addedWhileNearby: addedNearby
        )
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
                    ZStack(alignment: .bottom) {
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
                                        messageKey: row.rowId,
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
                                            name: resolvedName,
                                            displayId: formatUserId(userId: contact.userId)
                                        ).0,
                                        quoted: replyMetadata[replyMessageKey(message)]?.quoted,
                                        canReply: replyMetadata[replyMessageKey(message)]?.msgId != nil,
                                        reactions: row.reactions,
                                        grouping: row.grouping,
                                        timeLabel: row.timeLabel,
                                        voicePlayback: voicePlayback,
                                        arrivalLabel: row.arrivalLabel,
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
                                Color.clear
                                    .frame(height: 1)
                                    .id(conversationBottomId)
                                    .onAppear {
                                        isNearConversationBottom = true
                                        if newMessagesTargetRowId == nil {
                                            showNewMessages = false
                                        }
                                    }
                                    .onDisappear { isNearConversationBottom = false }
                            }
                            .padding(.horizontal, 12)
                            .padding(.vertical, 8)
                            .frame(minHeight: geo.size.height, alignment: .bottom)
                        }
                        .scrollDismissesKeyboard(.interactively)

                        if showNewMessages {
                            Button {
                                scrollToAnnouncedMessage(proxy: proxy)
                            } label: {
                                Label("New messages", systemImage: "arrow.down")
                                    .font(.subheadline.weight(.semibold))
                                    .padding(.horizontal, 14)
                                    .padding(.vertical, 9)
                                    .background(.regularMaterial, in: Capsule())
                            }
                            .buttonStyle(.plain)
                            .padding(.bottom, 12)
                            .accessibilityIdentifier("chat.new-messages")
                        }
                    }
                    .onChange(of: rows.map(\.rowId)) { rowIds in
                        handleConversationRowsChanged(rowIds, proxy: proxy)
                    }
                    .onAppear {
                        previousScrollRowIds = rows.map(\.rowId)
                        DispatchQueue.main.async {
                            scrollToLatest(proxy: proxy, animated: false)
                        }
                    }
                }
            }

            if identityCloneWarning {
                IdentityCloneNotice()
            }
            if let safetyFact {
                ContactSafetyNotice(
                    fact: safetyFact,
                    contactName: resolvedName,
                    onAcknowledge: { acknowledgeSafety(safetyFact) },
                    onCheckedOutOfBand: { clearSafetyQuarantine(safetyFact) }
                )
            }
            ComposerReachNotice(reach: composerReachVerdict, contactName: resolvedName)

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
                onDrawPhoto: {
                    guard let pendingPhoto else { return }
                    drawingPhoto = DrawingPhoto(jpeg: pendingPhoto)
                },
                onSend: sendCurrentDraft,
                onVoiceFinished: sendVoice,
                onVoiceError: { statusMessage = $0 }
            )
        }
        .navigationTitle(resolvedName)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if UITestConfiguration.scenario == .chatLateArrival {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Inject incoming message") {
                        UITestConfiguration.injectIncomingMessage(
                            contact: contact,
                            text: "New message while reading history"
                        )
                    }
                    .accessibilityIdentifier("chat.uitest-inject-incoming")
                }
            }
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
                .accessibilityIdentifier("chat.contact-details")
            }
        }
        .onAppear {
            draft = DraftStore.load(chatId: contact.userId)
            isMuted = ChatMuteStore.isMuted(contact.userId)
            isBlocked = (try? store.isUserBlocked(userId: contact.userId)) ?? false
            ChatVisibility.setVisible(contact.userId)
            MessageNotifier.clearChatNotifications(chatId: contact.userId)
            if !UITestConfiguration.isEnabled {
                MeshController.shared.notifyChatViewed(chatId: contact.userId)
            }
            reload()
            cancellable = ChatEvents.subject.sink { chatId in
                if chatId == contact.userId { reload() }
            }
        }
        .onDisappear {
            ChatVisibility.clearVisible(contact.userId)
            voiceRecorder.cancel()
            voicePlayback.stop()
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
                MessageNotifier.clearChatNotifications(chatId: contact.userId)
                if !UITestConfiguration.isEnabled {
                    MeshController.shared.notifyChatViewed(chatId: contact.userId)
                }
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
                    MeshController.shared.contactListChanged()
                },
                onReport: {
                    // Nil means a mail app took it. An address back means there
                    // was none -- it is already on the pasteboard, so say so
                    // rather than letting the button dead-end.
                    if let address = launchContactReport(
                        contact: displayContact,
                        reporterUserId: identity.userId
                    ) {
                        statusMessage = noMailAppMessage(address: address)
                    }
                },
                relayCardIsStale: connectivity.staleRelayContacts.contains(contact.userId),
                identityCloneWarning: identityCloneWarning,
                onShareContact: {
                    showDetails = false
                    // One sheet at a time: let the details sheet finish
                    // dismissing before the code takes its place.
                    DispatchQueue.main.async { shareContact = true }
                }
            ) {
                showDetails = false
                confirmDelete = true
            }
        }
        .sheet(isPresented: $shareContact) {
            // The stored contact, not `displayContact`: a shared card carries
            // the name and keys exactly as they gave them to us, never this
            // phone's private nickname for them.
            ShareContactView(contact: contact, identity: identity)
        }
        .fullScreenCover(item: $viewedPhoto) { photo in
            PhotoViewerOverlay(jpeg: photo.jpeg)
        }
        // Presented at the screen's outer level, like the photo viewer, so it
        // covers the whole conversation rather than nesting inside the composer.
        .fullScreenCover(item: $drawingPhoto) { photo in
            PhotoMarkupEditor(
                jpeg: photo.jpeg,
                onCancel: { drawingPhoto = nil },
                onConfirm: { annotated in
                    // Straight back into the staged slot, so the caption and
                    // reply target already in the composer are untouched. The
                    // editor has already re-run the size guard on these bytes.
                    pendingPhoto = annotated
                    drawingPhoto = nil
                }
            )
        }
        .alert("Delete contact?", isPresented: $confirmDelete) {
            Button("Delete", role: .destructive) {
                try? store.deleteContact(userId: contact.userId, nowMs: Int64(Date().timeIntervalSince1970 * 1000))
                FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
                MeshController.shared.contactListChanged()
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
        .accessibilityIdentifier("screen.chat")
    }

    private var canSend: Bool {
        pendingPhoto != nil || !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func sendCurrentDraft() {
        let replyToMsgId = replyingTo.flatMap { replyMetadata[replyMessageKey($0)]?.msgId }
        let messageSender = sender
        let outcome = ComposerSendPolicy.attempt(
            draft: draft,
            pendingPhoto: pendingPhoto,
            sendPhoto: { photo, caption in
                messageSender.sendAttachment(
                    contact: contact,
                    attachment: AttachmentPayload(
                        mediaType: .image,
                        mimeType: "image/jpeg",
                        durationMs: 0,
                        blob: photo,
                        caption: caption
                    ),
                    replyToMsgId: replyToMsgId
                )
            },
            sendText: { text in
                messageSender.sendText(contact: contact, text: text, replyToMsgId: replyToMsgId)
            }
        )
        // Assigned back unconditionally: the composer empties only when
        // `ComposerSendPolicy` says the message is durably queued.
        draft = outcome.draft
        pendingPhoto = outcome.pendingPhoto
        switch outcome.status {
        case .queued:
            replyingTo = nil
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            reload()
        case .notQueued:
            statusMessage = String(localized: "Couldn't send. Your message is still here.")
        case .nothingToSend:
            break
        }
    }

    private func reload() {
        let loadedMessages = (try? store.messagesForChat(chatId: contact.userId)) ?? []
        messages = loadedMessages
        rows = ChatRowModel.build(
            from: loadedMessages,
            ownUserId: identity.userId,
            lateArrivalMs: loadLateArrivalTimes(
                store: store,
                chatId: contact.userId,
                visibleMessages: loadedMessages.filter { isVisibleChatKind($0.kind) },
                ownUserId: identity.userId
            )
        )
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
        let provenance = try? store.getContactProvenance(userId: contact.userId)
        addedNearby = provenance?.addedNearby ?? false
        identityCloneWarning = (try? store.hasIdentityCloneWarning(userId: contact.userId)) ?? false
        safetyFact = latestSafetyFact(
            facts: (try? store.contactSafetyFacts(includeAcknowledged: false)) ?? [],
            personUserId: contact.userId
        )
    }

    /// Put §10.4's banner away. Acknowledging through `observedSeq` acknowledges
    /// everything at or below it, so a person who dismisses the banner is not
    /// handed the same contact's older news afterwards.
    private func acknowledgeSafety(_ fact: ContactSafetyFact) {
        _ = try? store.acknowledgeContactSafetyFacts(
            personUserId: fact.personUserId,
            throughObservedSeq: fact.observedSeq
        )
        safetyFact = nil
    }

    /// DL-2's fork resolution: a person who re-verified out of band says so, and
    /// core stops quarantining this contact's device list. Never resolved by
    /// arithmetic — there is no path to this call that is not a tap.
    private func clearSafetyQuarantine(_ fact: ContactSafetyFact) {
        _ = try? store.clearRosterQuarantine(personUserId: fact.personUserId)
        acknowledgeSafety(fact)
    }

    private func sendVoice(url: URL, durationMs: Int32) {
        defer { try? FileManager.default.removeItem(at: url) }
        guard let data = try? Data(contentsOf: url), !data.isEmpty else {
            statusMessage = String(localized: "Could not save that voice message")
            return
        }
        guard data.count <= AttachmentPayload.maxBlobBytes else {
            statusMessage = String(localized: "That voice message is too long to send. Try a shorter one.")
            return
        }
        sender.sendAttachment(
            contact: contact,
            attachment: AttachmentPayload(
                mediaType: .audio,
                mimeType: VoiceRecorder.plan.mimeType,
                durationMs: durationMs,
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
        return ChatListLogic.contactDisplayName(contact)
    }

    private func scrollToLatest(proxy: ScrollViewProxy, animated: Bool = true) {
        guard let last = rows.last else { return }
        if animated {
            withAnimation { proxy.scrollTo(last.rowId, anchor: .bottom) }
        } else {
            proxy.scrollTo(last.rowId, anchor: .bottom)
        }
    }

    private var conversationBottomId: String {
        "chat-bottom-\(UserIdHex.encode(contact.userId))"
    }

    private func handleConversationRowsChanged(_ rowIds: [String], proxy: ScrollViewProxy) {
        let decision = ConversationScrollPolicy.decide(
            previousRowIds: previousScrollRowIds,
            currentRowIds: rowIds,
            lateArrivalRowIds: Set(rows.filter { $0.arrivalLabel != nil }.map(\.rowId)),
            isNearBottom: isNearConversationBottom,
            newestIsOwnMessage: rows.last?.message.senderUserId == identity.userId
        )
        previousScrollRowIds = rowIds

        switch decision {
        case .none:
            break
        case .autoScroll:
            showNewMessages = false
            newMessagesTargetRowId = nil
            DispatchQueue.main.async { scrollToLatest(proxy: proxy) }
        case .showNewMessages(let targetRowId):
            showNewMessages = true
            newMessagesTargetRowId = targetRowId
        }
    }

    private func scrollToAnnouncedMessage(proxy: ScrollViewProxy) {
        if let target = newMessagesTargetRowId {
            withAnimation { proxy.scrollTo(target, anchor: .center) }
        } else {
            scrollToLatest(proxy: proxy)
        }
        showNewMessages = false
        newMessagesTargetRowId = nil
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
    /// "Arrived 5:14 PM", set only for a message spliced in above content that
    /// was already here -- see `core/src/late_arrival.rs`.
    let arrivalLabel: String?

    private static let dayFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "MMMM d, yyyy"
        f.locale = .current
        return f
    }()

    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.timeStyle = .short
        f.dateStyle = .none
        return f
    }()

    static func build(
        from messages: [StoredMessage],
        ownUserId: Data,
        lateArrivalMs: [String: Int64] = [:]
    ) -> [ChatRowModel] {
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
                reactions: reactionsByTarget[target.stableKey] ?? [],
                arrivalLabel: lateArrivalMs[lateArrivalRowKey(message)].map { arrival in
                    String(
                        format: String(localized: "Arrived %@"),
                        timeFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(arrival) / 1000))
                    )
                }
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
    /// Stable across reloads and scrolling; identifies a voice message to the
    /// conversation's player.
    let messageKey: String
    let isOwn: Bool
    let tick: TickStatus?
    let contactColor: Color
    let quoted: QuotedMessagePreview?
    let canReply: Bool
    let reactions: [ReactionSummary]
    let grouping: MessageGrouping
    let timeLabel: String
    /// The conversation's voice player. Not observed here: only the voice
    /// bubble itself redraws while a message plays.
    let voicePlayback: VoiceMemoPlaybackController
    var arrivalLabel: String? = nil
    var onStatus: (String) -> Void = { _ in }
    var onReact: (String) -> Void = { _ in }
    var onReply: () -> Void = {}
    var onPhotoTap: (Data) -> Void = { _ in }
    var onQuotedTap: (StoredMessage) -> Void = { _ in }
    @State private var showLegend = false
    @State private var showInfo = false

    /// The contact's devices, in their own list's order, read only when the info
    /// sheet is built. Two store reads per opened sheet and none per bubble.
    private var contactActiveDeviceIds: [Data] {
        (try? AppStore.get().contactActiveDeviceIds(personUserId: message.chatId)) ?? []
    }

    private var contactDeviceState: ContactDeviceState {
        (try? AppStore.get().contactDeviceState(
            personUserId: message.chatId,
            deviceId: message.senderDeviceId
        )) ?? .unknown
    }

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
                    if grouping.showTimestamp || tick != nil {
                        HStack(spacing: 5) {
                            Spacer(minLength: 0)
                            if grouping.showTimestamp {
                                Text(timeLabel)
                                    .font(.caption2)
                                    .foregroundStyle(
                                        (isOwn ? Color.white : Color.primary).opacity(0.7)
                                    )
                            }
                            if let tick {
                                SignalTickView(status: tick, tint: isOwn ? .white : .secondary)
                            }
                        }
                    }
                }
                .padding(10)
                // The contact tint is translucent, which reads fine over the
                // chat background but turns see-through in the lifted
                // context-menu preview (it renders on a clear backdrop, so
                // neighbouring bubbles bleed through the reacted message).
                // Compositing the tint over the system background keeps the
                // in-list color identical and makes the preview opaque.
                .background(
                    bubbleShape
                        .fill(Color(.systemBackground))
                        .overlay(
                            bubbleShape
                                .fill(isOwn ? Color.accentColor : contactColor.opacity(0.24))
                        )
                )
                .foregroundStyle(isOwn ? Color.white : Color.primary)
                // Restrict the system targeted preview to the actual bubble.
                // SwiftUI then performs the same source-frame-to-fitted-frame
                // spring that Signal's custom iOS context menu implements.
                .contentShape(.contextMenuPreview, bubbleShape)
                // Simultaneous, not `.onTapGesture`: an exclusive tap gesture
                // on the bubble swallows taps aimed at a link inside its text
                // (6.6), and the tick legend is not worth a dead link. Both
                // recognise, so tapping a link may also flash the legend --
                // it clears itself after two seconds.
                .simultaneousGesture(TapGesture().onEnded {
                    if tick != nil {
                        showLegend = true
                    }
                })
                .contextMenu {
                    MessageActionsMenu(
                        canReply: canReply,
                        copyText: messageCopyText(message),
                        imageData: messageImageData(message),
                        ownReaction: reactions.first(where: { $0.reactedByOwnUser })?.emoji,
                        onReact: onReact,
                        onReply: onReply,
                        onCopy: {
                            UIPasteboard.general.string = messageCopyText(message)
                            onStatus("Copied")
                        },
                        onStatus: onStatus,
                        onInfo: { showInfo = true }
                    )
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

                // Set only for a message spliced in above content already
                // here (core/src/late_arrival.rs). The bubble keeps the
                // sender's time; this says when it reached us.
                if let arrivalLabel {
                    Text(arrivalLabel)
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
            ) + deviceInfoRows(
                // §8: per-device detail lives here, on the info sheet, and nowhere
                // a person reaches without asking for it. The ticks above stay
                // any-device, which is the whole promise that a contact's device
                // count is invisible. `chatId` is the contact's user id in a 1:1
                // chat, which is the only kind of chat this bubble is used in.
                messageDeviceInfoLines(
                    isOwn: isOwn,
                    label: deviceLabelFor(
                        senderDeviceId: message.senderDeviceId,
                        activeDeviceIds: contactActiveDeviceIds,
                        state: contactDeviceState
                    ),
                    contactDeviceCount: contactActiveDeviceIds.count
                )
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
                        onOpen: onPhotoTap
                    )
                case .audio:
                    VoiceMemoPlayerView(
                        messageKey: messageKey,
                        blob: attachment.blob,
                        durationMs: attachment.durationMs,
                        playback: voicePlayback
                    )
                }
                if !attachment.caption.isEmpty {
                    MessageBodyText(
                        text: attachment.caption,
                        isOwn: isOwn,
                        onStatus: onStatus
                    )
                }
            } else {
                Text("Unsupported attachment")
            }
        } else {
            MessageBodyText(
                text: String(data: message.payload, encoding: .utf8) ?? "",
                isOwn: isOwn,
                onStatus: onStatus
            )
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

struct IdentityCloneNotice: View {
    var body: some View {
        Text("CruiseMesh saw two copies of this contact. The messages already on this phone were kept.")
            .font(.caption)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Color.red.opacity(0.12))
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            .padding(.horizontal, 12)
            .padding(.bottom, 4)
    }
}

/// The one place a person is guaranteed to look before typing: a persistent,
/// non-modal line above the composer saying which direction of this chat cannot
/// cross the internet. Renders nothing for `.fine`, which is every ordinary
/// chat.
///
/// Deliberately not an alert, a toast, or a row inside the contact sheet three
/// taps away. The failure it describes is silent -- messages sit at one tick
/// forever and no screen explains why -- so it has to be where the typing
/// happens, and it has to stay put.
struct ComposerReachNotice: View {
    let reach: ComposerReach
    let contactName: String

    private var text: String? {
        switch reach {
        case .fine:
            return nil
        case .repliesCannotReachMe:
            return "Your messages will reach \(contactName), but their replies only arrive when you're near each other. Set up a Shore Pass to get replies anywhere."
        case .theyCannotBeReached:
            return "Messages you send \(contactName) while you're apart wait in your family's Shore Pass mailbox until their phone picks them up. If their phone isn't set up with your pass, they'll wait until you're near each other."
        case .neitherDirectionWorks:
            return String(localized: "Neither phone has a Shore Pass, so messages only cross when you're near each other. Either of you can set one up.")
        }
    }

    var body: some View {
        if let text {
            Text(text)
                .font(.caption)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Color(.secondarySystemBackground))
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                .padding(.horizontal, 12)
                .padding(.bottom, 4)
        }
    }
}

/// Preview card for a photo that's been picked but not yet sent.
///
/// `onDraw` opens the markup editor (`specs/photo-markup.md`). This card is the
/// one entry point, which is what lets a single editor serve the gallery, the
/// camera, 1:1 chats and groups.
struct PendingPhotoPreview: View {
    let jpeg: Data
    let onRemove: () -> Void
    let onDraw: () -> Void

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
            Spacer(minLength: 4)
            Button(action: onDraw) {
                Label("Draw", systemImage: "pencil.tip")
                    .font(.subheadline.weight(.semibold))
                    .frame(minHeight: 44)
            }
            .accessibilityIdentifier("chat.pending-photo.draw")
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
    senderDisplayId: String? = nil,
    nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
) -> [MessageInfoRow] {
    let f = DateFormatter()
    f.dateFormat = "MMMM d, yyyy h:mm a"
    f.locale = .current
    let sentAt = f.string(from: Date(timeIntervalSince1970: TimeInterval(message.timestamp) / 1000))

    var rows: [MessageInfoRow] = [
        .sentence(isOwn ? "Sent by you" : "Received"),
    ]
    if let senderDisplayId {
        rows.append(.labeled(label: String(localized: "Sender ID"), value: senderDisplayId))
    }
    rows.append(.labeled(label: "Time", value: sentAt))

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

/// `messageDeviceInfoLines`'s output as sheet rows.
///
/// Kept apart from `messageInfoRows` so the mapping stays a pure function of core
/// facts with no store reads in it, and so the words live beside every other
/// thing a family reads. Mirrors Android's `deviceInfoRows`.
func deviceInfoRows(_ lines: [DeviceInfoLine]) -> [MessageInfoRow] {
    lines.map { line in
        switch line {
        case .sentFrom(let label):
            return .labeled(label: String(localized: "Sent from"), value: deviceLabelText(label))
        case .addressedTo(let deviceCount):
            return .sentence(
                String(localized: "Ticks mean any one of their \(deviceCount) devices got it.")
            )
        case .noDeviceDetail:
            return .sentence(
                String(localized: "This contact has not told this phone which devices they use.")
            )
        }
    }
}

func deviceLabelText(_ label: DeviceLabel) -> String {
    switch label {
    case .numbered(let position):
        return String(localized: "Their device \(position)")
    case .removed:
        return String(localized: "A device they have removed")
    case .unknown:
        return String(localized: "A device this phone does not know")
    }
}

func groupMessageInfoRows(
    message: StoredMessage,
    isOwn: Bool,
    tick: TickStatus?,
    arrival: MessageArrival? = nil,
    receiptState: GroupReceiptState,
    ownUserId: Data,
    senderName: (Data) -> String,
    outboundExpiryMs: Int64? = nil,
    senderDisplayId: String? = nil,
    nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
) -> [MessageInfoRow] {
    var rows = messageInfoRows(
        message: message,
        isOwn: isOwn,
        tick: tick,
        arrival: arrival,
        outboundExpiryMs: outboundExpiryMs,
        senderDisplayId: senderDisplayId,
        nowMs: nowMs
    )
    guard isOwn else { return rows }
    for member in receiptState.members {
        if member.memberUserId == ownUserId { continue }
        if member.addedAtMs > 0 && member.addedAtMs > message.timestamp { continue }
        let memberTick = tickStatusFor(
            lamport: message.lamport,
            deliveredThrough: member.deliveredThrough,
            readThrough: member.readThrough
        )
        let status: String
        switch memberTick {
        case .read: status = "Read"
        case .delivered: status = "Delivered"
        case .sent: status = "Waiting"
        }
        var value = status
        if let via = member.deliveredViaTransport {
            value += " · \(transportRouteText(via))"
        }
        rows.append(.labeled(label: senderName(member.memberUserId), value: value))
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
    case 0: return "direct Bluetooth"
    case 1: return "another device over Bluetooth"
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
    formatter.timeStyle = .short
    formatter.dateStyle = .none
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

/// The staged photo currently open in the markup editor
/// (`specs/photo-markup.md`), keyed so `.fullScreenCover(item:)` has something
/// `Identifiable` to present from -- the same shape as `ViewedPhoto`.
struct DrawingPhoto: Identifiable {
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

/// Chat photo: keeps native aspect ratio. Its parent bubble owns the unified
/// reaction/action menu, including contextual Save image.
struct ChatImageView: View {
    let jpeg: Data
    var onOpen: (Data) -> Void = { _ in }

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
                .accessibilityHint("Double-tap to view full screen; long-press for message options")
        } else {
            Text("Photo (could not display)")
        }
    }
}

/// A voice message in the timeline: play/pause, elapsed over total, and a
/// seekable bar. The total is the decoder's once a player exists, falling back
/// to the duration the sender stated until then — that fallback is display
/// only and is never a seek target.
///
/// A pure view of the conversation's playback state: it owns no player. The
/// conversation does, so a message keeps playing when this bubble scrolls out
/// of the `LazyVStack` and is disposed — including the automatic scroll when a
/// new message arrives — and the bubble picks the same state back up when it
/// scrolls into view again.
struct VoiceMemoPlayerView: View {
    /// Identifies the message across reloads and scrolling: the conversation's
    /// stable row key (sender and lamport), not this view's identity.
    let messageKey: String
    let blob: Data
    let durationMs: Int32
    @ObservedObject var playback: VoiceMemoPlaybackController

    private var state: VoiceBubbleState {
        playback.state(for: messageKey, manifestDurationMs: durationMs)
    }

    private var progressLabel: String {
        "\(Self.clock(state.elapsed)) / \(Self.clock(state.total))"
    }

    private var progress: Double {
        VoicePlaybackDisplay.progressFraction(
            positionMs: Int((state.elapsed * 1000).rounded(.down)),
            totalMs: Int((state.total * 1000).rounded(.down))
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack {
                Button {
                    playback.toggle(key: messageKey, blob: blob)
                } label: {
                    Image(systemName: state.isPlaying ? "pause.fill" : "play.fill")
                }
                .accessibilityLabel(state.isPlaying ? "Pause voice message" : "Play voice message")
                VStack(alignment: .leading, spacing: 3) {
                    Text(verbatim: progressLabel)
                        .font(.subheadline.monospacedDigit())
                        .accessibilityHidden(true)
                    VoiceMemoSeekBar(
                        progress: progress,
                        progressLabel: progressLabel,
                        onSeek: { playback.seek(key: messageKey, fraction: $0) }
                    )
                }
            }
            if state.failed {
                Text("Could not play that voice message")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private static func clock(_ seconds: TimeInterval) -> String {
        let whole = Int(seconds.rounded(.down))
        return "\(whole / 60):" + String(format: "%02d", whole % 60)
    }
}

/// Exclusive drag so a scrub is not also a swipe-to-reply or a context-menu
/// long-press. VoiceOver treats it as an adjustable with the elapsed/total
/// line as its spoken value.
private struct VoiceMemoSeekBar: View {
    var progress: Double
    var progressLabel: String
    var onSeek: (Double) -> Void
    /// `GestureState` goes false if the system cancels the drag, so the
    /// process-wide `VoiceSeekDrag` flag cannot leak and disable reply
    /// on every other bubble. `begin()` is still called from `onChanged`
    /// so the flag is set on the first pixel, not a frame later.
    @GestureState private var dragging = false

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(.primary.opacity(0.25))
                Capsule()
                    .fill(.primary)
                    .frame(width: max(4, geo.size.width * progress))
            }
            .contentShape(Rectangle())
            // High-priority so this drag wins over the bubble's
            // simultaneous swipe-to-reply. VoiceSeekDrag is set
            // synchronously because preference updates are a frame late.
            .highPriorityGesture(
                DragGesture(minimumDistance: 0)
                    .updating($dragging) { _, state, _ in
                        state = true
                    }
                    .onChanged { value in
                        if !VoiceSeekDrag.isActive {
                            VoiceSeekDrag.begin()
                        }
                        guard geo.size.width > 0 else { return }
                        onSeek(Double(value.location.x / geo.size.width))
                    }
            )
        }
        .frame(width: 120, height: 22)
        .accessibilityElement(children: .ignore)
        .accessibilityIdentifier("voice.seek")
        .accessibilityLabel(Text("Voice message position"))
        .accessibilityValue(Text(progressLabel))
        .accessibilityAdjustableAction { direction in
            let step = 0.1
            switch direction {
            case .increment: onSeek(min(1, progress + step))
            case .decrement: onSeek(max(0, progress - step))
            default: break
            }
        }
        .onChange(of: dragging) { isDragging in
            if !isDragging, VoiceSeekDrag.isActive {
                VoiceSeekDrag.end()
            }
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


