import SwiftUI

/// What this phone shows once its person has removed it (§10 step 5).
///
/// Terminal, and in front of everything else, because every other screen would be
/// a lie: the chat list of a device that cannot send, receive or acknowledge
/// anything is a phone claiming to be part of a family it is no longer in. The
/// same reason the Terms gate sits where it does.
///
/// Three sentences and no button, on purpose.
///
/// * What happened, without asking the person to work out what "removed" means
///   for their messages.
/// * That nothing is lost — their contacts, groups and messages are on the
///   phone they still use, which is the first thing anybody wants to know.
/// * The way back, which is a real reinstall rather than a button: DL-4 makes a
///   removed device id gone for good, so joining again means a fresh key on a
///   fresh install, and offering "Set up again" here would offer something the
///   core is right to refuse.
///
/// Nothing on it can be operated, so nothing on it can put a removed device back
/// on the air by accident.
///
/// Mirrors Android's `DeviceRemovedScreen.kt`.
struct DeviceRemovedView: View {
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("This device was removed")
                    .font(.largeTitle.weight(.bold))
                Text("This phone was removed from your devices, using another device of yours. It has stopped sending and receiving messages.")
                    .font(.body)
                    .foregroundStyle(.secondary)
                Text("Your contacts, groups and messages are still on the devices you kept.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Text("To use this phone again, remove the app, install it again, and set it up as a new device.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(24)
        }
        .accessibilityIdentifier("screen.device-removed")
    }
}
