import SwiftUI

/// The permissions step on its own, for the doors that arrive past the wizard.
///
/// "This is another of my devices" and "Restore from backup" both finish by
/// marking setup complete from underneath: the phone already holds this
/// person's contacts, groups and history, so the wizard genuinely has nothing
/// left to ask it — and for everything except permissions that is right. What
/// it left behind was a chat list on a phone that had never been asked for
/// Bluetooth or notifications, with the mesh off and nothing on screen saying
/// why; a two-phone session had to grant access by hand from Settings.
///
/// Deliberately the same content as the wizard's own slide rather than a second
/// telling of it: `PermissionsSlide` is shared, so the two cannot drift.
struct PermissionsSetupView: View {
    @ObservedObject var appModel: AppModel
    let onDone: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Text("Your phone is set up. One last thing before you start.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, 28)
                .padding(.top, 20)

            PermissionsSlide(
                onEnable: {
                    MessageNotifier.requestPermission()
                    // The one permission the mesh cannot run without is being
                    // asked for right here, so bring the mesh up as the answer
                    // lands rather than leaving it for a later screen to notice.
                    appModel.startMesh()
                }
            )

            VStack(spacing: 14) {
                Button("Start using CruiseMesh") {
                    // Asked once. Somebody who declines is not held here, and is
                    // not asked again on the next launch either — the chat
                    // list's blocking banner is where a missing grant lives
                    // after this.
                    OnboardingStore.markPermissionsStepDone()
                    appModel.startMesh()
                    onDone()
                }
                .buttonStyle(.borderedProminent)
                .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .padding(20)
            .background(.bar)
        }
        .accessibilityIdentifier("screen.permissions_setup")
    }
}
