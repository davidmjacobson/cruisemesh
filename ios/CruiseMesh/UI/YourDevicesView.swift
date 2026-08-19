import SwiftUI

/// One row of "Your devices": what the roster says, plus what this phone
/// privately calls it.
struct YourDeviceListItem: Identifiable, Equatable {
    let row: OwnDeviceRow
    let name: String
    let firstSeenMs: Int64?

    var id: String { row.deviceIdHex }
}

/// "Your devices" (`specs/multi-device-v1.md` §13 WP6).
///
/// The one screen that answers "which phones and tablets am I signed in on?", and
/// the door to both journeys that change the answer. It shows what the person's
/// own roster says and nothing it worked out for itself: which devices are
/// listed, which one is this one, which one approves new devices, and — from this
/// phone's own notes — what they have been called and when this phone first saw
/// them.
///
/// Deliberately not behind Advanced. §13's product bar puts what a family needs on
/// the surface and capability behind the door; a person who loses a phone needs to
/// find this in Settings without being told where to look.
///
/// Presented by `SettingsView`'s `NavigationLink`, so it supplies no
/// `NavigationStack` of its own — the same shape `SailChecklistView` and
/// `ConnectionDetailsView` already use from that screen.
///
/// Mirrors Android's `YourDevicesScreen.kt`.
struct YourDevicesView: View {
    let identity: Identity

    @State private var revision = 0
    @State private var renaming: YourDeviceListItem?
    @State private var renameText = ""
    @State private var removing: YourDeviceListItem?
    @State private var removalRunning = false
    @State private var removalOutcome: RemoveDeviceResult?

    var body: some View {
        // Read so a rename or a removal re-runs the two store reads below: the
        // roster is the fact, and re-reading it is how a removal that
        // half-finished shows up as what actually happened.
        let _ = revision
        let roster = loadRoster()
        let items = listItems(roster: roster)
        let shape = yourDevicesShape(hasRoster: roster != nil, rows: items.map(\.row))
        let canAdd = canAddDevice(hasRoster: roster != nil, rows: items.map(\.row))
        // The device that holds the signing role, named the way the person named
        // it. Both withheld states below point at it, because "use the other one"
        // is useless advice on a fleet of three.
        let approverName = items.first(where: { $0.row.approves })
            .map(deviceDisplayName) ?? String(localized: "This phone")

        Form {
            Section {
                Text(introText(shape))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                // §10.2 that could not be done. The removal confirmation
                // promises the removed phone loses the family mailbox, and on
                // the relays that refuse this device the re-key it never will —
                // so this is the one place that admits it. Read on every
                // revision, so it appears after the removal that earned it and
                // goes as soon as a later rotation clears it.
                if RelayRotationNoticeStore.blocked() {
                    Text("Your family's Shore Pass could not be changed, so a device you removed can still reach the family mailbox. Contact support for a new pass.")
                        .font(.callout)
                        .foregroundStyle(.red)
                }
            }

            if !items.isEmpty {
                Section {
                    ForEach(items) { item in
                        DeviceRow(
                            item: item,
                            // Why Remove is absent, said under the row it is
                            // absent from. The reason was already worked out and
                            // simply never shown, so a person looking for the
                            // button read a missing control as a fault in the app
                            // rather than a rule about which phone to use.
                            blocked: removeBlockedReason(
                                rows: items.map(\.row),
                                row: item.row
                            ),
                            approverName: approverName,
                            onRename: {
                                renameText = item.name
                                renaming = item
                            },
                            onRemove: { removing = item }
                        )
                    }
                } footer: {
                    Text("Names you give devices stay on this phone. Nobody else sees them.")
                }
            }

            // §9.5's signature is the approving device's, so the ceremony can
            // only be finished there. Offering the link anywhere else would walk
            // a person through a code, a camera and six digits and fail at the
            // last step; withholding it and saying which phone to use costs one
            // line.
            if canAdd {
                Section {
                    NavigationLink {
                        AddDeviceView(
                            identity: identity,
                            role: .approvingDevice,
                            expectedPersonId: nil,
                            onFinished: { revision += 1 }
                        )
                    } label: {
                        Text("Add a device")
                    }
                } footer: {
                    Text("Set up another phone or tablet as you. You will need both in front of you.")
                }
            } else {
                Section {
                    Text(addDeviceWithheldText(approverName: approverName))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .navigationTitle("Your devices")
        .accessibilityIdentifier("screen.your-devices")
        .alert("Name this device", isPresented: renamingBinding) {
            TextField("Name", text: $renameText)
            Button("Save") {
                if let renaming {
                    DeviceNameStore.setName(deviceIdHex: renaming.row.deviceIdHex, name: renameText)
                }
                renaming = nil
                revision += 1
            }
            Button("Cancel", role: .cancel) { renaming = nil }
        } message: {
            Text("Names you give devices stay on this phone. Nobody else sees them.")
        }
        .alert("Remove this device?", isPresented: removingBinding) {
            Button("Remove", role: .destructive) {
                guard let target = removing else { return }
                removing = nil
                startRemoval(of: target)
            }
            Button("Cancel", role: .cancel) { removing = nil }
        } message: {
            Text(removeConfirmationText(removing))
        }
        // A blocking overlay rather than an alert: the ceremony is mid-flight and
        // there is nothing useful a person could do to it from here, and an alert
        // with no buttons of its own would grow a dismiss button that lies.
        .overlay {
            if removalRunning {
                ZStack {
                    Color.black.opacity(0.25).ignoresSafeArea()
                    ProgressView {
                        Text("Removing…")
                    }
                    .padding(24)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
                }
            }
        }
        .alert(
            removalOutcomeTitle(removalOutcome),
            isPresented: outcomeBinding
        ) {
            Button("Done", role: .cancel) { removalOutcome = nil }
        } message: {
            Text(removalOutcomeDetail(removalOutcome))
        }
    }

    // MARK: - Reading the roster

    private func loadRoster() -> Roster? {
        (try? AppStore.get().ownRoster()) ?? nil
    }

    private func listItems(roster: Roster?) -> [YourDeviceListItem] {
        let ownDeviceId = DeviceKeyStore.load()?.deviceId
        // A never-linked install has no roster to read, but the sentence above
        // the list still says "this is the only device signed in as you" — so the
        // screen shows that device rather than an empty space under its own
        // claim. No badge and no Remove: nothing approves anything yet, and there
        // is nothing else to be left with.
        let rows: [OwnDeviceRow]
        if let roster {
            rows = ownDeviceRows(
                deviceIds: coreRosterDeviceIds(roster: roster),
                approvingDeviceId: roster.approvingDeviceId,
                ownDeviceId: ownDeviceId
            )
        } else {
            rows = [thisDeviceOnlyRow(ownDeviceId: ownDeviceId)]
        }
        return rows.map { row in
            YourDeviceListItem(
                row: row,
                name: DeviceNameStore.name(deviceIdHex: row.deviceIdHex) ?? "",
                // Read, never stamped: the stamp is written when a roster is
                // adopted or applied, so a device with no date is one this phone
                // met before it kept notes rather than one nobody has opened this
                // screen for.
                firstSeenMs: DeviceNameStore.firstSeenMs(deviceIdHex: row.deviceIdHex)
            )
        }
    }

    // MARK: - Removal

    private func startRemoval(of item: YourDeviceListItem) {
        removalRunning = true
        let deviceId = item.row.deviceId
        let signedInAs = identity
        Task { @MainActor in
            // Off the main thread: §10.1's commit re-seals the retained backlog
            // record by record before it adopts anything, so on a fleet that has
            // been dark for a fortnight this is real work and not a flag being set.
            let outcome = await Task.detached(priority: .userInitiated) {
                RemoveDeviceSession(identity: signedInAs).remove(deviceId: deviceId)
            }.value
            removalRunning = false
            removalOutcome = outcome
            revision += 1
        }
    }

    // MARK: - Copy

    private func introText(_ shape: YourDevicesShape) -> String {
        switch shape {
        case .neverLinked, .onlyThisDevice:
            return String(
                localized: "This is the only device signed in as you. Add another and they will share your contacts, groups and messages."
            )
        case .several:
            return String(
                localized: "These are signed in as you. They all get your messages and stay in step with each other."
            )
        }
    }

    private func removeConfirmationText(_ item: YourDeviceListItem?) -> String {
        removeDeviceConfirmationText(
            deviceName: item.map(deviceDisplayName) ?? String(localized: "This phone"),
            hasShorePass: RelayConfigStore.load() != nil
        )
    }

    private func removalOutcomeTitle(_ outcome: RemoveDeviceResult?) -> String {
        switch outcome {
        case .removed: return String(localized: "Device removed")
        default: return String(localized: "Device not removed")
        }
    }

    private func removalOutcomeDetail(_ outcome: RemoveDeviceResult?) -> String {
        switch outcome {
        case .removed(_, let siblings, let unresealable, let rotationQueued):
            // §10.1 re-seals the retained backlog to the survivors. A record it
            // could not re-seal is a message that will not arrive, and counting
            // it and then never saying so left the person with a clean "Device
            // removed" over a quiet loss.
            let lost = unresealable > 0
                ? "\n\n" + String(
                    localized: "A few messages that were still on their way could not be carried over."
                )
                : ""
            // §10.2's cost to the devices that were not here. The new mailbox
            // credential reaches a sibling over the fleet's own sealed sync,
            // which has no transport on either shell yet, so until it does the
            // honest instruction is the shipped one: show them the setup card
            // again. Only worth saying when there is a sibling to strand and a
            // pass to strand it from.
            let passCard = (rotationQueued && siblings > 0)
                ? "\n\n" + String(
                    localized: "Your other devices will need your Shore Pass setup card again. Open Shore Pass on this phone and show the code to each of them."
                )
                : ""
            if siblings > 0 {
                return String(
                    localized: "Your contacts will be told as they come back into range. Your other devices catch up the next time they are online."
                ) + passCard + lost
            }
            return String(
                localized: "Your contacts will be told about the change as they come back into range."
            ) + lost
        case .refused(let reason):
            return removeRefusalText(reason)
        case nil:
            return ""
        }
    }

    // MARK: - Bindings

    private var renamingBinding: Binding<Bool> {
        Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })
    }

    private var removingBinding: Binding<Bool> {
        Binding(get: { removing != nil }, set: { if !$0 { removing = nil } })
    }

    private var outcomeBinding: Binding<Bool> {
        Binding(get: { removalOutcome != nil }, set: { if !$0 { removalOutcome = nil } })
    }
}

/// §10.1 and §10.2 in family words: what the person loses, and what survives.
///
/// Everything here is a consequence the app will actually produce. The Shore
/// Pass sentence appears only when there is a pass to lose, and it says "as soon
/// as this phone is online" rather than "now" because that is the truth: the
/// mailbox credential is re-keyed by the next relay pass that reaches the relay,
/// so a removal made at sea is a removal whose relay half is still owed. A
/// confirmation that overstates what removal does is worse than one that says
/// less. A free function so both promises are directly testable rather than
/// reachable only by tapping through a `Form`.
///
/// The first line names §10 step 5's meeting, because until the removed phone
/// meets one of the devices that are still this person's it carries on as though
/// nothing happened — and this tap used to promise otherwise while doing nothing
/// at all to the other device.
func removeDeviceConfirmationText(deviceName: String, hasShorePass: Bool) -> String {
    let happens = hasShorePass
        ? removeDeviceMailboxText(deviceName: deviceName)
        : removeDeviceQuietText(deviceName: deviceName)
    let survives = String(
        localized: "Your contacts, groups and messages stay on your other devices. Messages already on the removed device stay on it."
    )
    let undo = String(
        localized: "You cannot undo this. If you use that device again later, set it up as a new device."
    )
    return "\(happens)\n\n\(survives)\n\n\(undo)"
}

/// The first sentence when this person has no Shore Pass: §10.1 and step 5, and
/// nothing at all about a mailbox they do not have.
private func removeDeviceQuietText(deviceName: String) -> String {
    String(
        localized: "\(deviceName) stops staying in step with your other devices now. The next time it is on the same Wi-Fi as one of them, it stops sending and receiving messages altogether and says it was removed."
    )
}

/// The first sentence when there is a pass to lose: §10.2 takes the family
/// mailbox away from that phone too, on the first relay pass that can reach the
/// relay. Two functions rather than one sentence with a branch inside it, so
/// each wording is one catalog key and each can be read whole.
private func removeDeviceMailboxText(deviceName: String) -> String {
    String(
        localized: "\(deviceName) stops staying in step with your other devices now, and loses your family's Shore Pass mailbox as soon as this phone is online. The next time it is on the same Wi-Fi as one of them, it stops sending and receiving messages altogether and says it was removed."
    )
}

/// Why a removal was refused, in words that name the next thing to do.
func removeRefusalText(_ reason: RemoveDeviceRefusal) -> String {
    switch reason {
    case .noDevices, .noDeviceKeys:
        return String(localized: "This phone is not set up with any other devices yet.")
    case .notTheApprovingDevice:
        return String(
            localized: "Only the device that approves new devices can remove one. Use that device instead."
        )
    case .inboxKeyMissing:
        return String(
            localized: "This device has not caught up with an earlier change yet. Let it get online, then try again."
        )
    case .earlierRemovalUnfinished:
        return String(
            localized: "An earlier removal on this device was interrupted and has not finished. Contact support before removing anything else."
        )
    case .coreRefused:
        return String(localized: "Something went wrong and nothing was changed. Try again.")
    }
}

/// Why Remove is missing from a row, for a surface that wants to say so.
/// Why Remove is missing from a row, in words that name the next thing to do.
///
/// `.notTheApprovingDevice` is the one that needs more than a rule stated back at
/// the person: it says which device can do it, and what to do when that device is
/// the one that is gone — which is the situation they are usually in when they
/// came looking for Remove in the first place.
func removeBlockText(_ block: RemoveDeviceBlock, approverName: String) -> String {
    switch block {
    case .notTheApprovingDevice:
        return String(
            localized: "Only \(approverName) can remove a device. Open Your devices there. If that device is lost or broken, contact support — this one cannot remove devices on its own."
        )
    case .isTheApprovingDevice:
        return String(localized: "This device approves new devices, so it cannot remove itself.")
    case .lastDevice:
        return String(localized: "You cannot remove your only device.")
    }
}

/// Why **Add a device** is not on offer here, naming the phone that can.
func addDeviceWithheldText(approverName: String) -> String {
    String(
        localized: "Only \(approverName) can add a device. Open Your devices there. If that device is lost or broken, contact support."
    )
}

func deviceDisplayName(_ item: YourDeviceListItem) -> String {
    if !item.name.isEmpty { return item.name }
    if item.row.isThisDevice { return String(localized: "This phone") }
    return String(localized: "Device \(item.row.position)")
}

private struct DeviceRow: View {
    let item: YourDeviceListItem
    let blocked: RemoveDeviceBlock?
    let approverName: String
    var onRename: () -> Void
    var onRemove: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(deviceDisplayName(item))
                .font(.body)
            if item.row.approves {
                Text("Approves new devices")
                    .font(.caption)
                    .foregroundStyle(.tint)
            }
            if !item.row.deviceIdHex.isEmpty {
                Text(deviceCodeText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let seenText {
                Text(seenText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            HStack(spacing: 20) {
                Button("Rename") { onRename() }
                    .accessibilityIdentifier("device.rename.\(item.row.deviceIdHex)")
                if item.row.removable {
                    Button("Remove", role: .destructive) { onRemove() }
                        .accessibilityIdentifier("device.remove.\(item.row.deviceIdHex)")
                }
            }
            .buttonStyle(.borderless)
            .font(.callout)
            .padding(.top, 2)
            if !item.row.removable, let blocked {
                Text(removeBlockText(blocked, approverName: approverName))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 4)
    }

    private var deviceCodeText: String {
        String(localized: "Code \(shortDeviceCode(item.row.deviceIdHex))")
    }

    private var seenText: String? {
        guard let firstSeenMs = item.firstSeenMs else { return nil }
        let date = Date(timeIntervalSince1970: Double(firstSeenMs) / 1_000)
        let formatted = date.formatted(date: .long, time: .omitted)
        return String(localized: "Seen here since \(formatted)")
    }
}
