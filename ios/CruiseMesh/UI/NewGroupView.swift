import SwiftUI

struct NewGroupView: View {
    let identity: Identity
    let contacts: [Contact]
    let onCreated: (Group) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var selectedIds = Set<Data>()
    @State private var error: String?

    private var canCreate: Bool {
        !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !selectedIds.isEmpty
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Group") {
                    TextField("Group name", text: $name)
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
                                HStack {
                                    AvatarView(userId: contact.userId, name: contact.name, size: 40)
                                    Text(ChatListLogic.displayNameOrId(
                                        name: contact.name,
                                        displayId: formatUserId(userId: contact.userId)
                                    ))
                                    .foregroundStyle(.primary)
                                    Spacer()
                                    if selectedIds.contains(contact.userId) {
                                        Image(systemName: "checkmark.circle.fill")
                                            .foregroundStyle(Color.accentColor)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle("New group")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") { createGroupChat() }
                        .disabled(!canCreate)
                }
            }
            .alert("Could not create group", isPresented: Binding(
                get: { error != nil },
                set: { if !$0 { error = nil } }
            )) {
                Button("OK", role: .cancel) { error = nil }
            } message: {
                Text(error ?? "")
            }
        }
    }

    private func toggle(_ userId: Data) {
        if selectedIds.contains(userId) {
            selectedIds.remove(userId)
        } else {
            selectedIds.insert(userId)
        }
    }

    private func createGroupChat() {
        let members = contacts.filter { selectedIds.contains($0.userId) }
        let sender = GroupSender(store: AppStore.get(), identity: identity)
        guard let group = sender.createAndInvite(name: name, members: members) else {
            error = "Check the group name and selected members, then try again."
            return
        }
        onCreated(group)
        dismiss()
    }
}
