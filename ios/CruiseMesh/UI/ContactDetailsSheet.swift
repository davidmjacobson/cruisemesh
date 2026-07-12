import SwiftUI

/// Contact detail sheet (Android `ContactDetailsSheet` parity): avatar, ID,
/// safety-word fingerprint verification, and delete.
struct ContactDetailsSheet: View {
    let contact: Contact
    var avatarData: Data? = nil
    let onDelete: () -> Void
    @Environment(\.dismiss) private var dismiss

    private var displayId: String { formatUserId(userId: contact.userId) }
    private var displayName: String {
        ChatListLogic.displayNameOrId(name: contact.name, displayId: displayId)
    }
    private var fingerprint: [String] { fingerprintWords(userId: contact.userId) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 0) {
                    AvatarView(
                        userId: contact.userId,
                        name: contact.name,
                        size: 72,
                        photo: avatarData.flatMap { UIImage(data: $0) }
                    )
                        .padding(.top, 8)

                    Text(displayName)
                        .font(.title2.weight(.semibold))
                        .padding(.top, 16)

                    Text(displayId)
                        .font(.body.monospaced())
                        .multilineTextAlignment(.center)
                        .padding(.top, 8)

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
        }
    }
}
