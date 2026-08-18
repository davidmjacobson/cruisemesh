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
                // Recorded here rather than where the bytes are built: a
                // backup that was prepared and then cancelled is not one
                // anybody could restore from, and the checklist step says a
                // backup exists.
                BackupCreatedStore.markCreated()
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

/// Opening a `.cmbak` on a fresh install, which since `specs/multi-device-v1.md`
/// §9 is a fork rather than one action.
///
/// **"Replace this device"** is the old meaning, unchanged, and needs the phone
/// in the backup switched off — it is the §1 clone if it is not. **"Set up as a
/// new device"** hands off to §9's ceremony instead, so the person ends with two
/// phones that stay in step rather than two that fight over one author stream.
///
/// Both branches are core's `CoreRestorePlan`, not this screen's opinion. What
/// this screen owes is the words and the ordering, and it takes both from there.
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
    /// Nil until the person picks, which is what keeps the old single-meaning
    /// Restore button from being reachable before they have.
    @State private var chosenIntent: CoreRestoreIntent?
    @State private var linkPersonId: Data?

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
                    }
                    RestoreIntentFork(
                        plans: preview.plans,
                        chosen: $chosenIntent,
                        onSetUpAsNewDevice: { personId in linkPersonId = personId }
                    )
                    // What to bring over is a question only replacing asks:
                    // setting up as a new device takes §9.3's export from the
                    // other phone, not this file's contents.
                    if chosenIntent == .replaceThisDevice {
                        Section {
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
                    chosenIntent = nil
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
            // `navigationDestination(isPresented:)` rather than the `item:`
            // overload, which is iOS 17 and this target is 16.
            .navigationDestination(isPresented: linkPresented) {
                // The person id read out of the `.cmbak`, handed to the ceremony
                // so core can refuse a code shown by anybody else's phone.
                // `loadOrCreate`, because this is a fresh install being adopted
                // and it needs keys of its own to talk with. The person root
                // inside the backup is never adopted here — §14.2 keeps it in the
                // `.cmbak`, and `LinkSession` says why a linked phone is a reader.
                AddDeviceView(
                    identity: IdentityStore.loadOrCreate(),
                    role: .newDevice,
                    expectedPersonId: linkPersonId,
                    onFinished: {
                        onStaged()
                        dismiss()
                    }
                )
            }
        }
    }

    private var linkPresented: Binding<Bool> {
        Binding(get: { linkPersonId != nil }, set: { if !$0 { linkPersonId = nil } })
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

/// §9's fork: the two things "restore" can mean, said in family words.
///
/// The branches, their order and every consequence stated beside them come from
/// `core_backup_restore_plans`. In particular "Set up as a new device" is listed
/// first because core lists it first, and it does so deliberately: it is the
/// choice that cannot produce §1's clone, and a person who has both phones in
/// front of them should read it before the one that requires switching the old
/// one off.
///
/// `cloneHazardIfSourceIsLive` is what turns the second branch's warning on. It
/// is a fact core carries on the plan rather than a rule this screen knows, so a
/// future intent with different hazards gets the right warning without this file
/// being edited.
///
/// Mirrors Android's `RestoreIntentFork`.
struct RestoreIntentFork: View {
    let plans: [CoreRestorePlan]
    @Binding var chosen: CoreRestoreIntent?
    var onSetUpAsNewDevice: (Data) -> Void

    var body: some View {
        if !plans.isEmpty {
            Section("What is this device?") {
                ForEach(Array(plans.enumerated()), id: \.offset) { entry in
                    let plan = entry.element
                    VStack(alignment: .leading, spacing: 4) {
                        Button {
                            chosen = plan.intent
                            // Routing happens on the tap, not on a second
                            // confirm: the ceremony is itself a long,
                            // cancellable, confirmed journey, and a confirm
                            // before a confirm teaches people to tap through
                            // both.
                            if plan.routesToLinkCeremony { onSetUpAsNewDevice(plan.personId) }
                        } label: {
                            Text(title(plan.intent))
                                .fontWeight(chosen == plan.intent ? .bold : .regular)
                        }
                        .buttonStyle(.borderless)
                        Text(detail(plan.intent))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        if plan.cloneHazardIfSourceIsLive {
                            Text("Only do this if the old device is switched off. Two devices using one backup will get out of step.")
                                .font(.caption)
                                .foregroundStyle(.red)
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
        }
    }

    private func title(_ intent: CoreRestoreIntent) -> String {
        switch intent {
        case .linkAsNewDevice: return String(localized: "Set up as a new device")
        case .replaceThisDevice: return String(localized: "Replace this device")
        }
    }

    private func detail(_ intent: CoreRestoreIntent) -> String {
        switch intent {
        case .linkAsNewDevice:
            return String(
                localized: "This device joins the ones you already use. You will need your other device in front of you."
            )
        case .replaceThisDevice:
            return String(
                localized: "This device takes over from the one in the backup. Use this when the old one is gone or broken."
            )
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
