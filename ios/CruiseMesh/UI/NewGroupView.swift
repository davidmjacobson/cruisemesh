import SwiftUI

/// Create a group: name + multi-select friends (DESIGN.md §14.6 / §6.5).
struct NewGroupView: View {
    let identity: Identity
    let onDone: (Group) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var selected: Set<Data> = []
    @State private var contacts: [Contact] = []

    private let store = AppStore.get()

    private var canCreate: Bool {
        !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !selected.isEmpty
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Group name") {
                    TextField("Name", text: $name)
                }
                Section("Members") {
                    if contacts.isEmpty {
                        Text("Add friends before creating a group.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(contacts, id: \.userId) { contact in
                            Button {
                                toggle(contact.userId)
                            } label: {
                                HStack(spacing: 12) {
                                    Image(systemName: selected.contains(contact.userId)
                                        ? "checkmark.circle.fill"
                                        : "circle")
                                        .foregroundStyle(selected.contains(contact.userId) ? Color.accentColor : .secondary)
                                    AvatarView(
                                        userId: contact.userId,
                                        name: contact.name,
                                        size: 36,
                                        photo: (try? AppStore.get().contactAvatar(userId: contact.userId))
                                            .flatMap { UIImage(data: $0) }
                                    )
                                    Text(ChatListLogic.displayNameOrId(
                                        name: contact.name,
                                        displayId: formatUserId(userId: contact.userId)
                                    ))
                                    .foregroundStyle(.primary)
                                    Spacer()
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle("New group")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create (\(selected.count + 1))") { create() }
                        .disabled(!canCreate)
                }
            }
            .onAppear {
                contacts = (try? store.listContacts()) ?? []
            }
        }
    }

    private func toggle(_ userId: Data) {
        if selected.contains(userId) {
            selected.remove(userId)
        } else {
            selected.insert(userId)
        }
    }

    private func create() {
        let members = contacts.filter { selected.contains($0.userId) }
        let sender = GroupSender(store: store, identity: identity)
        guard let group = sender.createAndInvite(name: name, members: members) else { return }
        dismiss()
        onDone(group)
    }
}
