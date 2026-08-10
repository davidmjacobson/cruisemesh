import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct BackupDocument: FileDocument {
    static var readableContentTypes: [UTType] { [UTType(filenameExtension: "cmbak") ?? .data] }
    var data: Data

    init(data: Data = Data()) { self.data = data }
    init(configuration: ReadConfiguration) throws {
        data = configuration.file.regularFileContents ?? Data()
    }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

struct BackupExportView: View {
    @State private var passphrase = ""
    @State private var confirmation = ""
    @State private var exporting = false
    @State private var document = BackupDocument()
    @State private var showExporter = false
    @State private var error: String?
    @State private var inventory: BackupInventory?
    @State private var includeHistory = true
    @State private var includeCourier = false
    @State private var backupSaved = false

    private var acceptable: Bool {
        passphrase.count >= Int(backupMinPassphraseLength()) && passphrase == confirmation
    }

    var body: some View {
        Form {
            Section("Backup contents") {
                Toggle("Include my message history", isOn: $includeHistory)
                Text(historyInventoryText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Toggle("Include pending deliveries for others", isOn: $includeCourier)
                Text(courierInventoryText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("Your identity, contacts, groups, privacy settings, and message-number continuity are always included.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Protect your backup") {
                BackupPassphraseField(
                    "Passphrase",
                    text: $passphrase,
                    contentType: .newPassword,
                    accessibilityIdentifier: "backup.export.passphrase"
                )
                BackupPassphraseField(
                    "Confirm passphrase",
                    text: $confirmation,
                    contentType: .newPassword,
                    accessibilityIdentifier: "backup.export.confirmation"
                )
                Text("Use at least \(backupMinPassphraseLength()) characters. You need this passphrase to restore the file.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                LabeledContent("Strength", value: strengthLabel)
            }
            Section {
                Button(exporting ? "Preparing backup…" : "Save encrypted backup") {
                    createBackup()
                }
                .disabled(!acceptable || exporting)
            }
            if let error {
                Section { Text(error).foregroundStyle(.red) }
            }
        }
        .navigationTitle("Back up account")
        .task {
            inventory = try? await Task.detached { try BackupService.inventory() }.value
        }
        .fileExporter(
            isPresented: $showExporter,
            document: document,
            contentType: BackupDocument.readableContentTypes[0],
            defaultFilename: BackupService.suggestedFileName
        ) { result in
            switch result {
            case .success:
                error = nil
                backupSaved = true
            case .failure(let failure):
                error = backupFailureText(failure, fallback: .couldNotSave).text
            }
        }
        .alert("Backup saved", isPresented: $backupSaved) {
            Button("Done", role: .cancel) {}
        } message: {
            Text("Keep the backup file and its passphrase somewhere safe.")
        }
    }

    private var strengthLabel: String {
        switch backupPassphraseStrength(passphrase: passphrase) {
        case .tooShort: return "Too short"
        case .weak: return "Weak"
        case .fair: return "Fair"
        case .strong: return "Strong"
        }
    }

    private var historyInventoryText: String {
        guard let inventory else { return "Counting messages…" }
        return "\(inventory.messageCount) messages · \(formatBackupBytes(inventory.messageBytes)); " +
            "\(inventory.pendingOwnDeliveryCount) pending deliveries from me."
    }

    private var courierInventoryText: String {
        guard let inventory else { return "Counting encrypted courier messages…" }
        return "\(inventory.pendingCourierDeliveryCount) encrypted messages · " +
            "\(formatBackupBytes(inventory.pendingCourierDeliveryBytes)). " +
            "They are unreadable on this device."
    }

    private func createBackup() {
        exporting = true
        error = nil
        let secret = passphrase
        Task {
            do {
                let options = BackupContentOptions(
                    includeMessageHistory: includeHistory,
                    includePendingDeliveriesForOthers: includeCourier
                )
                let data = try await Task.detached {
                    try BackupService.buildBackup(passphrase: secret, options: options)
                }.value
                document = BackupDocument(data: data)
                showExporter = true
            } catch {
                self.error = backupFailureText(error, fallback: .couldNotSave).text
            }
            exporting = false
        }
    }
}

struct BackupRestoreView: View {
    var onStaged: () -> Void = {}
    @Environment(\.dismiss) private var dismiss
    @State private var file = Data()
    @State private var fileName = ""
    @State private var passphrase = ""
    @State private var showImporter = false
    @State private var restoring = false
    @State private var error: String?
    @State private var restartRequired = false
    @State private var preview: BackupPreview?
    @State private var includeHistory = true
    @State private var includeCourier = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Backup file") {
                    Button(file.isEmpty ? "Choose .cmbak file" : "Choose a different file") {
                        showImporter = true
                    }
                    if !fileName.isEmpty { Text(fileName).font(.caption).foregroundStyle(.secondary) }
                }
                Section("Unlock backup") {
                    BackupPassphraseField(
                        "Passphrase",
                        text: $passphrase,
                        contentType: .password,
                        accessibilityIdentifier: "backup.restore.passphrase"
                    )
                        .onChange(of: passphrase) { _ in preview = nil }
                    if preview == nil {
                        Button(restoring ? "Reviewing…" : "Review backup") { review() }
                            .disabled(file.isEmpty || passphrase.isEmpty || restoring)
                    }
                }
                if let preview {
                    Section("Restore preview") {
                        Text("\(preview.inventory.contactCount) contacts · \(preview.inventory.groupCount) groups")
                        Toggle("Restore my message history", isOn: $includeHistory)
                        Text("\(preview.inventory.messageCount) messages · \(formatBackupBytes(preview.inventory.messageBytes))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Toggle("Restore pending deliveries for others", isOn: $includeCourier)
                        Text("\(preview.inventory.pendingCourierDeliveryCount) encrypted messages · \(formatBackupBytes(preview.inventory.pendingCourierDeliveryBytes))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text("Identity, contacts, groups, privacy settings, and message-number continuity will always be restored.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Button(restoring ? "Restoring…" : "Restore account") { restore() }
                        .disabled(file.isEmpty || passphrase.isEmpty || restoring)
                    }
                }
                if let error { Section { Text(error).foregroundStyle(.red) } }
            }
            .navigationTitle("Restore from backup")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
            .fileImporter(
                isPresented: $showImporter,
                allowedContentTypes: BackupDocument.readableContentTypes
            ) { result in
                do {
                    let url = try result.get()
                    let scoped = url.startAccessingSecurityScopedResource()
                    defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                    file = try BackupService.readBackupFile(at: url)
                    fileName = url.lastPathComponent
                    preview = nil
                    error = nil
                } catch {
                    self.error = backupFailureText(error, fallback: .couldNotReadFile).text
                }
            }
            .alert("Restore ready", isPresented: $restartRequired) {
                Button("Done") {
                    onStaged()
                    dismiss()
                }
            } message: {
                Text("Close and reopen CruiseMesh to finish installing the restored account.")
            }
        }
    }

    private func review() {
        restoring = true
        error = nil
        let selected = file
        let secret = passphrase
        Task {
            do {
                preview = try await Task.detached {
                    try BackupService.previewBackup(file: selected, passphrase: secret)
                }.value
            } catch {
                self.error = backupFailureText(error, fallback: .couldNotRestore).text
            }
            restoring = false
        }
    }

    private func restore() {
        restoring = true
        error = nil
        let selected = file
        let secret = passphrase
        let options = BackupContentOptions(
            includeMessageHistory: includeHistory,
            includePendingDeliveriesForOthers: includeCourier
        )
        Task {
            do {
                try await Task.detached {
                    try BackupService.stageRestore(
                        file: selected,
                        passphrase: secret,
                        options: options
                    )
                }.value
                restartRequired = true
            } catch {
                self.error = backupFailureText(error, fallback: .couldNotRestore).text
            }
            restoring = false
        }
    }
}

private struct BackupPassphraseField: View {
    let title: String
    @Binding var text: String
    let contentType: UITextContentType
    let accessibilityIdentifier: String

    @State private var isRevealed = false
    @FocusState private var isFocused: Bool

    init(
        _ title: String,
        text: Binding<String>,
        contentType: UITextContentType,
        accessibilityIdentifier: String
    ) {
        self.title = title
        _text = text
        self.contentType = contentType
        self.accessibilityIdentifier = accessibilityIdentifier
    }

    var body: some View {
        HStack(spacing: 8) {
            if isRevealed {
                TextField(title, text: $text)
                    .textContentType(contentType)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .focused($isFocused)
                    .accessibilityIdentifier(accessibilityIdentifier)
            } else {
                SecureField(title, text: $text)
                    .textContentType(contentType)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .focused($isFocused)
                    .accessibilityIdentifier(accessibilityIdentifier)
            }

            Button {
                isRevealed.toggle()
                DispatchQueue.main.async { isFocused = true }
            } label: {
                Image(systemName: isRevealed ? "eye.slash" : "eye")
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(isRevealed ? "Hide passphrase" : "Show passphrase")
            .accessibilityIdentifier("\(accessibilityIdentifier).visibility")
        }
    }
}

private func formatBackupBytes(_ bytes: UInt64) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
}
