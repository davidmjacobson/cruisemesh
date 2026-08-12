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
    /// Opens the markup editor on the staged photo (`specs/photo-markup.md`).
    let onDrawPhoto: () -> Void
    let onSend: () -> Void
    let onVoiceFinished: (URL, Int32) -> Void
    let onVoiceError: (String) -> Void
    @State private var showPhotoLibrary = false
    @State private var photoLibraryAttempt = PhotoLibraryPickerAttempt()
    /// True while a voice message is being recorded. The composer hands the
    /// whole row to the recording control then and takes the text field away —
    /// which is also what keeps a hands-free recording alive: typing one
    /// character used to flip `canSend`, replace the recording control with
    /// Send, and discard the recording through its `.onDisappear`.
    @State private var voiceCapturing = false

    var body: some View {
        VStack(spacing: 8) {
            if let replyingToPreview, !voiceCapturing {
                ReplyComposerPreview(preview: replyingToPreview, onCancel: onCancelReply)
            }
            if let pendingPhoto, !voiceCapturing {
                PendingPhotoPreview(
                    jpeg: pendingPhoto,
                    onRemove: onRemovePhoto,
                    onDraw: onDrawPhoto
                )
            }
            HStack(alignment: .bottom, spacing: 8) {
                if !voiceCapturing {
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
                }

                if canSend && !voiceCapturing {
                    Button(action: onSend) {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.system(size: 32, weight: .semibold))
                    }
                    .accessibilityLabel("Send")
                    .accessibilityIdentifier("chat.composer.send")
                } else {
                    HoldToRecordButton(
                        recorder: voiceRecorder,
                        isCapturing: $voiceCapturing,
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

/// The voice-message recording sheet shared by `ChatView` and `GroupChatView`
/// (FI12). Owns its own presentation/recording bindings so the sheet can
/// close itself on send, cancel, or a "mic unavailable" failure; the caller
/// only needs to know what to do with the finished recording.
///
/// This is the plain Start/Stop route for anyone who cannot hold a button, so
/// it drives the *same* core capture state as the composer's gesture: the
/// duration bound, the byte budget, and the accidental-tap floor all apply
/// here too. Leaving it to the recorder's own hard backstop instead would let
/// this path run past what the copy above the buttons promises.
struct VoiceMemoRecorderSheet: View {
    let voiceRecorder: VoiceRecorder
    @Binding var isPresented: Bool
    @Binding var isRecording: Bool
    let onSend: (URL, Int32) -> Void
    let onMicUnavailable: () -> Void
    var onRecordingFailed: () -> Void = {}

    @State private var capture = voiceCaptureIdleState()
    @State private var startedAt: TimeInterval = 0

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
                        apply(voiceCaptureFinish(state: capture, elapsedMs: elapsedMs()))
                    }
                    .buttonStyle(.borderedProminent)
                } else {
                    Button("Start recording") {
                        let started = voiceCaptureStartHandsFree(state: capture)
                        guard started.effect == .start, voiceRecorder.start() else {
                            onMicUnavailable()
                            isPresented = false
                            return
                        }
                        startedAt = ProcessInfo.processInfo.systemUptime
                        capture = started.state
                        isRecording = true
                    }
                    .buttonStyle(.borderedProminent)
                }
                Spacer()
            }
            .padding(24)
            .navigationTitle("Voice message")
            .navigationBarTitleDisplayMode(.inline)
            .task(id: isRecording) {
                while !Task.isCancelled, isRecording {
                    try? await Task.sleep(nanoseconds: 200_000_000)
                    guard isRecording else { return }
                    apply(voiceCaptureElapsed(state: capture, elapsedMs: elapsedMs()))
                    guard isRecording else { return }
                    apply(voiceCaptureBytes(state: capture, bytesWritten: voiceRecorder.bytesRecorded()))
                }
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { isPresented = false }
                }
            }
        }
    }

    /// Monotonic, not wall time: a clock correction mid-recording must not
    /// change what the user recorded.
    private func elapsedMs() -> UInt32 {
        let millis = (ProcessInfo.processInfo.systemUptime - startedAt) * 1000
        return UInt32(max(0, min(millis, Double(UInt32.max))))
    }

    private func apply(_ step: CoreVoiceCaptureStep) {
        capture = step.state
        switch step.effect {
        case .send:
            isRecording = false
            // Dismiss only once the file is finalized: the sheet's onDismiss
            // calls `cancel()`, which would otherwise delete the recording out
            // from under this finalize. By the time the completion runs the
            // recorder has already cleared its state, so that cancel is a no-op.
            voiceRecorder.stop { result in
                if let (url, duration) = result {
                    onSend(url, duration)
                } else {
                    onRecordingFailed()
                }
                isPresented = false
            }
        case .discardTooShort, .discardCancelled:
            voiceRecorder.cancel()
            isRecording = false
            isPresented = false
        case .start, VoiceCaptureEffect.none:
            break
        }
    }
}

/// The photo-library/camera/voice-message attachment pipeline shared by
/// `ChatView` and `GroupChatView` (FI12): load+compress a picked photo,
/// present the camera, present the voice-message sheet. Each call site differs
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
                            onAttachmentError(String(localized: "Could not prepare photo"))
                            photoItem = nil
                            return
                        }
                        Self.log.info("Loaded selected photo (\(data.count, privacy: .public) bytes)")
                        guard let jpeg = MediaCompressor.compressImage(data: data) else {
                            Self.log.error(
                                "Could not decode or compress selected photo (\(data.count, privacy: .public) bytes)"
                            )
                            onAttachmentError(String(localized: "Could not prepare photo"))
                            photoItem = nil
                            return
                        }
                        Self.log.info("Prepared selected photo (\(jpeg.count, privacy: .public) JPEG bytes)")
                        onPhotoReady(jpeg)
                    } catch {
                        Self.log.error(
                            "Could not load selected photo: \(error.localizedDescription, privacy: .public)"
                        )
                        onAttachmentError(String(localized: "Could not prepare photo"))
                    }
                    photoItem = nil
                }
            }
            .sheet(isPresented: $showCamera) {
                CameraPicker { image in
                    if let jpeg = MediaCompressor.compress(image: image) {
                        onPhotoReady(jpeg)
                    } else {
                        onAttachmentError(String(localized: "Could not prepare photo"))
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
                    onMicUnavailable: { onAttachmentError("Microphone unavailable") },
                    onRecordingFailed: {
                        // `onAttachmentError` copy is shown via `Text(String)`, which
                        // does not auto-localize, so resolve the catalog key here to
                        // match the sibling `String(localized:)` use in
                        // `HoldToRecordButton` for the same message.
                        onAttachmentError(String(localized: "Couldn't record that voice message. Try again."))
                    }
                )
            }
    }
}

extension View {
    /// Attaches the shared photo/camera/voice-message pipeline (FI12). See
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
