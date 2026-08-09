import OSLog
import PhotosUI
import SwiftUI
import UIKit

struct PhotoLibraryPickerAttempt: Equatable {
    enum Dismissal: Equatable {
        case selected
        case cancelled
    }

    private(set) var receivedSelection = false

    mutating func begin() {
        receivedSelection = false
    }

    mutating func selected() {
        receivedSelection = true
    }

    var dismissal: Dismissal {
        receivedSelection ? .selected : .cancelled
    }
}

/// The message-composer row shared by `ChatView` and `GroupChatView` (FI12):
/// reply/pending-photo previews, the attach menu, the draft field, and the
/// send/hold-to-record button were ~identical copy-pasted blocks between the
/// two screens. The two call sites differ only in what happens when the user
/// sends or starts recording, which are supplied as closures.
struct ChatComposerBar: View {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "PhotoLibrary")

    let replyingToPreview: QuotedMessagePreview?
    let pendingPhoto: Data?
    @Binding var draft: String
    @Binding var photoItem: PhotosPickerItem?
    @Binding var showCamera: Bool
    @Binding var showVoice: Bool
    var composerFocused: FocusState<Bool>.Binding
    let voiceRecorder: VoiceRecorder
    let canSend: Bool
    let onCancelReply: () -> Void
    let onRemovePhoto: () -> Void
    let onSend: () -> Void
    let onVoiceFinished: (URL, Int32) -> Void
    let onVoiceError: (String) -> Void
    @State private var showPhotoLibrary = false
    @State private var photoLibraryAttempt = PhotoLibraryPickerAttempt()

    var body: some View {
        VStack(spacing: 8) {
            if let replyingToPreview {
                ReplyComposerPreview(preview: replyingToPreview, onCancel: onCancelReply)
            }
            if let pendingPhoto {
                PendingPhotoPreview(jpeg: pendingPhoto, onRemove: onRemovePhoto)
            }
            HStack(alignment: .bottom, spacing: 8) {
                Menu {
                    Button {
                        photoLibraryAttempt.begin()
                        Self.log.info("Photo library picker requested from the chat attachment menu")
                        // Let the Menu's popover dismiss before presenting the
                        // system picker. Presenting a PhotosPicker directly
                        // from that transient popover fails on iPad.
                        DispatchQueue.main.async {
                            showPhotoLibrary = true
                        }
                    } label: {
                        Label("Photo library", systemImage: "photo")
                    }
                    .accessibilityIdentifier("chat.attach.photo-library")
                    Button { showCamera = true } label: {
                        Label("Take photo", systemImage: "camera")
                    }
                    Button { showVoice = true } label: {
                        Label("Voice message", systemImage: "mic")
                    }
                } label: {
                    Image(systemName: "plus.circle.fill")
                        .font(.system(size: 28))
                }
                .accessibilityLabel("Attach")

                TextField("Message", text: $draft, axis: .vertical)
                    .accessibilityIdentifier("chat.composer.text")
                    .lineLimit(1...4)
                    .focused(composerFocused)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background(
                        Capsule(style: .continuous)
                            .fill(Color(uiColor: .secondarySystemBackground))
                    )

                if canSend {
                    Button(action: onSend) {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.system(size: 32, weight: .semibold))
                    }
                    .accessibilityLabel("Send")
                    .accessibilityIdentifier("chat.composer.send")
                } else {
                    HoldToRecordButton(
                        recorder: voiceRecorder,
                        onFinished: onVoiceFinished,
                        onError: onVoiceError,
                        onAccessibilityFallback: { showVoice = true }
                    )
                }
            }
        }
        .padding(12)
        .background(.bar)
        .photosPicker(
            isPresented: $showPhotoLibrary,
            selection: $photoItem,
            matching: .images
        )
        .onChange(of: showPhotoLibrary) { isPresented in
            if isPresented {
                Self.log.info("Photo library picker presentation became active")
            } else {
                switch photoLibraryAttempt.dismissal {
                case .selected:
                    Self.log.info("Photo library picker dismissed after a selection")
                case .cancelled:
                    Self.log.info("Photo library picker dismissed without a selection")
                }
            }
        }
        .onChange(of: photoItem) { item in
            guard item != nil else { return }
            photoLibraryAttempt.selected()
            Self.log.info("Photo library picker returned a selection")
        }
    }
}

/// The voice-memo recording sheet shared by `ChatView` and `GroupChatView`
/// (FI12). Owns its own presentation/recording bindings so the sheet can
/// close itself on send, cancel, or a "mic unavailable" failure; the caller
/// only needs to know what to do with the finished recording.
struct VoiceMemoRecorderSheet: View {
    let voiceRecorder: VoiceRecorder
    @Binding var isPresented: Bool
    @Binding var isRecording: Bool
    let onSend: (URL, Int32) -> Void
    let onMicUnavailable: () -> Void

    var body: some View {
        NavigationStack {
            VStack(spacing: 24) {
                Image(systemName: isRecording ? "waveform.circle.fill" : "mic.circle")
                    .font(.system(size: 72))
                    .foregroundStyle(isRecording ? Color.red : Color.accentColor)
                Text(isRecording ? "Recording…" : "Voice message")
                    .font(.title2.weight(.semibold))
                Text("Voice messages stop automatically after \(Int(VoiceRecorder.maxDurationSeconds)) seconds.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                if isRecording {
                    Button("Stop and send") {
                        if let (url, duration) = voiceRecorder.stop() {
                            onSend(url, duration)
                        }
                        isRecording = false
                        isPresented = false
                    }
                    .buttonStyle(.borderedProminent)
                } else {
                    Button("Start recording") {
                        if voiceRecorder.start() {
                            isRecording = true
                        } else {
                            onMicUnavailable()
                            isPresented = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                }
                Spacer()
            }
            .padding(24)
            .navigationTitle("Voice message")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { isPresented = false }
                }
            }
        }
    }
}

/// The photo-library/camera/voice-memo attachment pipeline shared by
/// `ChatView` and `GroupChatView` (FI12): load+compress a picked photo,
/// present the camera, present the voice-memo sheet. Each call site differs
/// only in where the resulting JPEG/audio lands and how failures are
/// surfaced, supplied as closures.
private struct AttachmentPickerModifiers: ViewModifier {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "PhotoLibrary")

    @Binding var photoItem: PhotosPickerItem?
    @Binding var showCamera: Bool
    @Binding var showVoice: Bool
    @Binding var voiceRecording: Bool
    let voiceRecorder: VoiceRecorder
    let onPhotoReady: (Data) -> Void
    let onAttachmentError: (String) -> Void
    let onVoiceSend: (URL, Int32) -> Void

    func body(content: Content) -> some View {
        content
            .onChange(of: photoItem) { item in
                guard let item else { return }
                Task {
                    do {
                        guard let data = try await item.loadTransferable(type: Data.self) else {
                            Self.log.error("Selected photo did not provide transferable bytes")
                            onAttachmentError("Could not prepare photo")
                            photoItem = nil
                            return
                        }
                        Self.log.info("Loaded selected photo (\(data.count, privacy: .public) bytes)")
                        guard let jpeg = MediaCompressor.compressImage(data: data) else {
                            Self.log.error(
                                "Could not decode or compress selected photo (\(data.count, privacy: .public) bytes)"
                            )
                            onAttachmentError("Could not prepare photo")
                            photoItem = nil
                            return
                        }
                        Self.log.info("Prepared selected photo (\(jpeg.count, privacy: .public) JPEG bytes)")
                        onPhotoReady(jpeg)
                    } catch {
                        Self.log.error(
                            "Could not load selected photo: \(error.localizedDescription, privacy: .public)"
                        )
                        onAttachmentError("Could not prepare photo")
                    }
                    photoItem = nil
                }
            }
            .sheet(isPresented: $showCamera) {
                CameraPicker { image in
                    if let jpeg = MediaCompressor.compress(image: image) {
                        onPhotoReady(jpeg)
                    } else {
                        onAttachmentError("Could not prepare photo")
                    }
                }
            }
            .sheet(isPresented: $showVoice, onDismiss: {
                voiceRecorder.cancel()
                voiceRecording = false
            }) {
                VoiceMemoRecorderSheet(
                    voiceRecorder: voiceRecorder,
                    isPresented: $showVoice,
                    isRecording: $voiceRecording,
                    onSend: onVoiceSend,
                    onMicUnavailable: { onAttachmentError("Microphone unavailable") }
                )
            }
    }
}

extension View {
    /// Attaches the shared photo/camera/voice-memo pipeline (FI12). See
    /// `AttachmentPickerModifiers`.
    func chatAttachmentPipeline(
        photoItem: Binding<PhotosPickerItem?>,
        showCamera: Binding<Bool>,
        showVoice: Binding<Bool>,
        voiceRecording: Binding<Bool>,
        voiceRecorder: VoiceRecorder,
        onPhotoReady: @escaping (Data) -> Void,
        onAttachmentError: @escaping (String) -> Void,
        onVoiceSend: @escaping (URL, Int32) -> Void
    ) -> some View {
        modifier(AttachmentPickerModifiers(
            photoItem: photoItem,
            showCamera: showCamera,
            showVoice: showVoice,
            voiceRecording: voiceRecording,
            voiceRecorder: voiceRecorder,
            onPhotoReady: onPhotoReady,
            onAttachmentError: onAttachmentError,
            onVoiceSend: onVoiceSend
        ))
    }
}
