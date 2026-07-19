import SwiftUI
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

    private var acceptable: Bool {
        passphrase.count >= Int(backupMinPassphraseLength()) && passphrase == confirmation
    }

    var body: some View {
        Form {
            Section("Protect your backup") {
                SecureField("Passphrase", text: $passphrase)
                SecureField("Confirm passphrase", text: $confirmation)
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
        .fileExporter(
            isPresented: $showExporter,
            document: document,
            contentType: BackupDocument.readableContentTypes[0],
            defaultFilename: BackupService.suggestedFileName
        ) { result in
            if case .failure(let failure) = result { error = failure.localizedDescription }
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

    private func createBackup() {
        exporting = true
        error = nil
        let secret = passphrase
        Task {
            do {
                let data = try await Task.detached { try BackupService.buildBackup(passphrase: secret) }.value
                document = BackupDocument(data: data)
                showExporter = true
            } catch {
                self.error = error.localizedDescription
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
                    SecureField("Passphrase", text: $passphrase)
                    Button(restoring ? "Restoring…" : "Restore account") { restore() }
                        .disabled(file.isEmpty || passphrase.isEmpty || restoring)
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
                    error = nil
                } catch {
                    self.error = error.localizedDescription
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

    private func restore() {
        restoring = true
        error = nil
        let selected = file
        let secret = passphrase
        Task {
            do {
                try await Task.detached {
                    try BackupService.stageRestore(file: selected, passphrase: secret)
                }.value
                restartRequired = true
            } catch {
                self.error = error.localizedDescription
            }
            restoring = false
        }
    }
}
