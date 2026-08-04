import SwiftUI

struct HelpSupportView: View {
    let appModel: AppModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section("Shore Pass") {
                    NavigationLink {
                        CruisePassView(initialCard: nil, appModel: appModel)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Set up or fix Shore Pass")
                            Text("Open the setup link from your purchase email. CruiseMesh checks and saves it automatically.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    Text("If the link does not open, copy the setup card, then go to Settings → Shore Pass and choose Paste and set up.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section("Delivery") {
                    NavigationLink {
                        ConnectionDetailsView()
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Understand delivery")
                            Text("See active paths, per-person history, recent activity, and diagnostic sharing.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                Section("More help") {
                    Link("Open CruiseMesh support", destination: URL(string: "https://cruisemesh.app/support/")!)
                    Text("Never post a Shore Pass setup card or relay token publicly.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Help & support")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
    }
}
