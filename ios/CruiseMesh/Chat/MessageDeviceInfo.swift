import Foundation

/// §8's receipt rule, at the one place it is allowed to be visible.
///
/// `specs/multi-device-v1.md` §8: "delivered/read shown to contacts is
/// **any-device**; per-device receipt detail lives behind Advanced." The ticks in
/// a chat are therefore untouched — one tick, two ticks, blue ticks, exactly as
/// before, because a person messages a *person* and how many phones they carry is
/// a §2 non-goal to expose. What Advanced gets is the detail behind that: which
/// device of theirs a received message came from, and how many of their devices
/// a sent message is being delivered to.
///
/// # An honest limit, stated once
///
/// There is no per-device *receipt* in the core, and this file does not invent
/// one. Watermarks are person-and-chat keyed (`recordOutgoingReceipt`), which is
/// precisely what makes "any-device" fall out for free once any sibling records
/// the receipt — and it means nothing anywhere knows that a message reached their
/// tablet but not their phone. So the sent-message line says how many devices the
/// message is addressed to and that a tick means any of them, which is true,
/// rather than a per-device table that would be fiction.
///
/// Received messages are different: `StoredMessage.senderDeviceId` is a real
/// fact, recorded per row, and it is what the received line names.
///
/// The exact twin of Android's `MessageDeviceInfo.kt`.
enum DeviceInfoLine: Equatable {
    /// "Sent from: their second device" — a received message's origin.
    case sentFrom(DeviceLabel)

    /// "Ticks mean any one of their N devices got it" — the honest shape of an
    /// any-device receipt.
    case addressedTo(deviceCount: Int)

    /// The contact has never told us about their devices (every legacy peer).
    case noDeviceDetail
}

/// How to name one device of a contact, without ever naming a person's hardware.
enum DeviceLabel: Equatable {
    /// Their Nth device, 1-based, in the order their own list gives.
    case numbered(position: Int)

    /// A device they have since removed (DL-4: tombstoned, and stays so).
    case removed

    /// A device id that no list of theirs names — including every message from a
    /// peer that has never sent one, which is every build in the field today.
    case unknown
}

/// Which of a contact's devices `senderDeviceId` is.
///
/// - Parameters:
///   - activeDeviceIds: `MessageStore.contactActiveDeviceIds` for this contact,
///     in their roster's own order — so "their second device" means the same
///     thing on every phone that holds the same list.
///   - state: `MessageStore.contactDeviceState` for the id, which is the only
///     thing that can tell a device we have simply never heard of from one that
///     was deliberately removed.
func deviceLabelFor(
    senderDeviceId: Data?,
    activeDeviceIds: [Data],
    state: ContactDeviceState
) -> DeviceLabel {
    guard let senderDeviceId, !senderDeviceId.isEmpty else { return .unknown }
    if let index = activeDeviceIds.firstIndex(of: senderDeviceId) {
        return .numbered(position: index + 1)
    }
    return state == .revoked ? .removed : .unknown
}

/// The Advanced-only lines for one message, or an empty list when there is
/// nothing true to add.
///
/// - Parameters:
///   - isOwn: whether this person sent it.
///   - label: which of the contact's devices sent it, for a received message.
///   - contactDeviceCount: how many devices the contact currently has, 0 for a
///     contact who has never told us.
func messageDeviceInfoLines(
    isOwn: Bool,
    label: DeviceLabel,
    contactDeviceCount: Int
) -> [DeviceInfoLine] {
    // A person with one device is the single-device world this whole spec
    // promises to keep invisible (§2 goal 1). Saying "any of their 1 devices"
    // would be leaking device count in the one case where there is nothing to
    // know.
    if isOwn {
        return contactDeviceCount > 1 ? [.addressedTo(deviceCount: contactDeviceCount)] : []
    }
    if label == .unknown {
        return contactDeviceCount == 0 ? [.noDeviceDetail] : []
    }
    return [.sentFrom(label)]
}
