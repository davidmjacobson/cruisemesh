import SwiftUI

struct HelpSupportView: View {
    let appModel: AppModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section("Cruise Pass") {
                    NavigationLink {
                        CruisePassView(initialCard: nil, appModel: appModel)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Set up or fix Cruise Pass")
                            Text("Paste the CMRELAY1 card from your purchase email, review it, then test and use it.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    Text("If a setup link does not open, go to Settings → Cruise Pass, paste the complete card, choose Review, then Test and use.")
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
                    Text("Never post a Cruise Pass setup card or relay token publicly.")
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
