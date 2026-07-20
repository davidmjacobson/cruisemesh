import SwiftUI

/// Contact detail sheet (Android `ContactDetailsSheet` parity): avatar, ID,
/// safety-word fingerprint verification, and delete.
struct ContactDetailsSheet: View {
    let contact: Contact
    var avatarData: Data? = nil
    var reachability: ReachabilityLevel? = nil
    var connectivityText: String? = nil
    var isMuted: Bool = false
    var onMutedChange: (Bool) -> Void = { _ in }
    var onSetNickname: (String?) -> Void = { _ in }
    let onDelete: () -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var editingNickname = false
    @State private var nicknameDraft = ""

    private var displayId: String { formatUserId(userId: contact.userId) }
    private var displayName: String {
        ChatListLogic.displayNameOrId(name: coreContactDisplayName(contact: contact), displayId: displayId)
    }
    private var hasNickname: Bool {
        !(contact.nickname ?? "").trimmingCharacters(in: .whitespaces).isEmpty
    }
    private var fingerprint: [String] { fingerprintWords(userId: contact.userId) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 0) {
                    AvatarView(
                        userId: contact.userId,
                        name: displayName,
                        size: 72,
                        photo: avatarData.flatMap { UIImage(data: $0) },
                        reachability: reachability
                    )
                        .padding(.top, 8)

                    Text(displayName)
                        .font(.title2.weight(.semibold))
                        .padding(.top, 16)

                    if hasNickname {
                        Text("Also known as \(contact.name)")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .padding(.top, 4)
                    }

                    Text(displayId)
                        .font(.body.monospaced())
                        .multilineTextAlignment(.center)
                        .padding(.top, 8)

                    Button(hasNickname ? "Edit nickname" : "Add a nickname") {
                        nicknameDraft = contact.nickname ?? ""
                        editingNickname = true
                    }
                    .buttonStyle(.bordered)
                    .padding(.top, 12)

                    if let connectivityText {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Connectivity")
                                .font(.title3.weight(.semibold))
                            Text(connectivityText)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(20)
                        .background(
                            RoundedRectangle(cornerRadius: 24, style: .continuous)
                                .strokeBorder(Color.secondary.opacity(0.35), lineWidth: 1)
                        )
                        .padding(.top, 24)
                    }

                    Toggle("Mute notifications", isOn: Binding(
                        get: { isMuted },
                        set: onMutedChange
                    ))
                    .padding(.top, 24)

                    VStack(alignment: .leading, spacing: 12) {
                        Text("Safety words")
                            .font(.title3.weight(.semibold))

                        HStack(spacing: 8) {
                            ForEach(fingerprint, id: \.self) { word in
                                Text(word)
                                    .font(.subheadline.weight(.medium))
                                    .frame(maxWidth: .infinity)
                                    .padding(.vertical, 12)
                                    .background(
                                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                                            .strokeBorder(Color.secondary.opacity(0.35), lineWidth: 1)
                                    )
                            }
                        }

                        Text("Read these aloud to verify.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(20)
                    .background(
                        RoundedRectangle(cornerRadius: 24, style: .continuous)
                            .strokeBorder(Color.secondary.opacity(0.35), lineWidth: 1)
                    )
                    .padding(.top, 24)

                    Button(role: .destructive, action: onDelete) {
                        Text("Delete contact")
                            .frame(maxWidth: .infinity)
                            .frame(height: 52)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(.red)
                    .padding(.top, 24)
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
            .navigationTitle("Details")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .alert("Nickname", isPresented: $editingNickname) {
                TextField(contact.name, text: $nicknameDraft)
                Button("Save") {
                    let trimmed = nicknameDraft.trimmingCharacters(in: .whitespaces)
                    onSetNickname(trimmed.isEmpty ? nil : trimmed)
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Only you see this. It replaces the name they shared, just on your phone.")
            }
        }
    }
}
