import SwiftUI

struct TermsAcceptanceView: View {
    let onAccept: () -> Void

    @Environment(\.openURL) private var openURL
    @State private var agreed = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("Before you start")
                    .font(.largeTitle.bold())
                Text("CruiseMesh is person-to-person messaging. Use it lawfully, connect only with people you trust, and do not send abusive or prohibited content.")
                    .font(.title3)
                Text("Messages are end-to-end encrypted, so CruiseMesh cannot proactively read them. You can block a contact and report abuse from their contact details.")
                    .foregroundStyle(.secondary)

                Toggle(
                    "I have read and agree to the Terms of Use and Privacy Policy.",
                    isOn: $agreed
                )
                .toggleStyle(.switch)
                .padding(.top, 8)

                Button("I agree", action: onAccept)
                    .buttonStyle(.borderedProminent)
                    .disabled(!agreed)
                    .frame(maxWidth: .infinity)

                HStack {
                    Spacer()
                    Button("Terms of Use") { openURL(TermsAcceptanceStore.termsURL) }
                    Button("Privacy policy") { openURL(TermsAcceptanceStore.privacyURL) }
                    Spacer()
                }
                .buttonStyle(.borderless)
            }
            .padding(28)
        }
    }
}
