import AVFoundation
import Combine
import PhotosUI
import SwiftUI
import UIKit

struct ChatView: View {
    let contact: Contact
    let identity: Identity

    @Environment(\.dismiss) private var dismiss
    @State private var messages: [StoredMessage] = []
    @State private var deliveredThrough: UInt64 = 0
    @State private var readThrough: UInt64 = 0
    @State private var draft = ""
    @State private var showAttach = false
    @State private var showVoice = false
    @State private var showDetails = false
    @State private var confirmDelete = false
    @State private var photoItem: PhotosPickerItem?
    @State private var showCamera = false
    @State private var statusMessage: String?
    @State private var cancellable: AnyCancellable?
    @State private var voiceRecorder = VoiceRecorder()
    @State private var voiceRecording = false
    @State private var voiceElapsed: Int = 0

    private let store = AppStore.get()
    private var sender: RealMeshSender { RealMeshSender(store: store, identity: identity) }

    private var visible: [StoredMessage] {
        messages.filter { isVisibleChatKind($0.kind) }
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 4) {
                        ForEach(Array(visible.enumerated()), id: \.offset) { index, message in
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
                                ).0
                            )
                            .id(message.lamport)
                        }
                    }
                    .padding(.horizontal, 12)
                }
                .onChange(of: visible.count) { _ in
                    if let last = visible.last {
                        withAnimation { proxy.scrollTo(last.lamport, anchor: .bottom) }
                    }
                }
            }

            HStack(alignment: .center, spacing: 8) {
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
                        .font(.title2)
                }

                TextField("Message", text: $draft, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...4)

                Button("Send") {
                    let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else { return }
                    sender.sendText(contact: contact, text: text)
                    draft = ""
                    reload()
                }
                .buttonStyle(.borderedProminent)
            }
            .padding(12)
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
                        AvatarView(userId: contact.userId, name: contact.name, size: 32)
                        VStack(alignment: .leading) {
                            Text(ChatListLogic.displayNameOrId(
                                name: contact.name,
                                displayId: formatUserId(userId: contact.userId)
                            ))
                            .font(.headline)
                            Text("Contact details")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .buttonStyle(.plain)
            }
        }
        .onAppear {
            ChatVisibility.setVisible(contact.userId)
            MeshController.shared.notifyChatViewed(chatId: contact.userId)
            reload()
            cancellable = ChatEvents.subject.sink { chatId in
                if chatId == contact.userId { reload() }
            }
        }
        .onDisappear {
            ChatVisibility.setVisible(nil)
            voiceRecorder.cancel()
        }
        .onChange(of: photoItem) { item in
            guard let item else { return }
            Task {
                if let data = try? await item.loadTransferable(type: Data.self),
                   let jpeg = MediaCompressor.compressImage(data: data) {
                    sender.sendAttachment(
                        contact: contact,
                        attachment: AttachmentPayload(
                            mediaType: .image,
                            mimeType: "image/jpeg",
                            durationMs: 0,
                            blob: jpeg
                        )
                    )
                    reload()
                } else {
                    statusMessage = "Could not prepare photo"
                }
                photoItem = nil
            }
        }
        .sheet(isPresented: $showCamera) {
            CameraPicker { image in
                if let jpeg = MediaCompressor.compress(image: image) {
                    sender.sendAttachment(
                        contact: contact,
                        attachment: AttachmentPayload(
                            mediaType: .image,
                            mimeType: "image/jpeg",
                            durationMs: 0,
                            blob: jpeg
                        )
                    )
                    reload()
                } else {
                    statusMessage = "Could not prepare photo"
                }
            }
        }
        .sheet(isPresented: $showDetails) {
            ContactDetailsSheet(contact: contact) {
                showDetails = false
                confirmDelete = true
            }
        }
        .alert("Delete contact?", isPresented: $confirmDelete) {
            Button("Delete", role: .destructive) {
                try? store.deleteContact(userId: contact.userId)
                dismiss()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Removes the contact and chat history.")
        }
        .alert("Voice memo", isPresented: $showVoice) {
            if voiceRecording {
                Button("Send") {
                    if let (url, duration) = voiceRecorder.stop() {
                        sendVoice(url: url, durationMs: duration)
                    }
                    voiceRecording = false
                    showVoice = false
                }
            } else {
                Button("Start") {
                    if voiceRecorder.start() {
                        voiceRecording = true
                    } else {
                        statusMessage = "Microphone unavailable"
                    }
                }
            }
            Button("Cancel", role: .cancel) {
                voiceRecorder.cancel()
                voiceRecording = false
            }
        } message: {
            Text(voiceRecording ? "Recording… tap Send when done (max 60s)." : "Tap Start, then Send.")
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

    private func reload() {
        messages = (try? store.messagesForChat(chatId: contact.userId)) ?? []
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
            )
        )
        reload()
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
        f.locale = Locale(identifier: "en_US")
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1000))
    }
}

private struct MessageBubbleView: View {
    let message: StoredMessage
    let isOwn: Bool
    let tick: TickStatus?
    let contactColor: Color
    @State private var showLegend = false

    var body: some View {
        HStack {
            if isOwn { Spacer(minLength: 40) }
            VStack(alignment: isOwn ? .trailing : .leading, spacing: 4) {
                VStack(alignment: .leading, spacing: 6) {
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
                    RoundedRectangle(cornerRadius: 18, style: .continuous)
                        .fill(isOwn ? Color.accentColor : contactColor.opacity(0.24))
                )
                .foregroundStyle(isOwn ? Color.white : Color.primary)
                .onTapGesture {
                    if tick != nil { showLegend = true }
                }

                Text(timeLabel(message.timestamp))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            if !isOwn { Spacer(minLength: 40) }
        }
        .padding(.vertical, 2)
        .alert("Message status", isPresented: $showLegend) {
            Button("OK", role: .cancel) {}
        } message: {
            if let tick { Text(tickLegendText(tick)) }
        }
    }

    @ViewBuilder
    private var content: some View {
        if message.kind == ProtocolKind.attachmentManifest {
            if let attachment = AttachmentPayload.decode(message.payload) {
                switch attachment.mediaType {
                case .image:
                    if let ui = UIImage(data: attachment.blob) {
                        Image(uiImage: ui)
                            .resizable()
                            .scaledToFit()
                            .frame(maxHeight: 240)
                            .clipShape(RoundedRectangle(cornerRadius: 12))
                    } else {
                        Text("Photo (could not display)")
                    }
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
        f.locale = Locale(identifier: "en_US")
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }
}

private struct VoiceMemoPlayerView: View {
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
                        let url = FileManager.default.temporaryDirectory
                            .appendingPathComponent("play-\(UUID().uuidString).m4a")
                        try blob.write(to: url)
                        let p = try AVAudioPlayer(contentsOf: url)
                        p.play()
                        player = p
                        playing = true
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

private struct CameraPicker: UIViewControllerRepresentable {
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

private struct ContactDetailsSheet: View {
    let contact: Contact
    let onDelete: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section("Contact") {
                    LabeledContent("Name", value: contact.name)
                    LabeledContent("ID", value: formatUserId(userId: contact.userId))
                    LabeledContent(
                        "Fingerprint",
                        value: fingerprintWords(userId: contact.userId).joined(separator: " ")
                    )
                }
                Section {
                    Button("Delete contact", role: .destructive, action: onDelete)
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
