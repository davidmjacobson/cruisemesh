import Foundation

/// "Your devices", as rows (`specs/multi-device-v1.md` §13 WP6).
///
/// The exact twin of Android's `OwnDevicesModel.kt`. Everything here is
/// arithmetic over facts the core already decided: which device ids a roster
/// names, which one holds the roster-signing role, and which one this install
/// is. No policy is invented — in particular the two refusals below are core's,
/// written down here only so the screen can decline to offer a tap that
/// `coreRevokeDevicesRoster` would refuse after the person had already read a
/// confirmation dialog.
struct OwnDeviceRow: Equatable {
    let deviceId: Data
    /// Full 32-character hex. Never shown whole — see `shortDeviceCode`.
    let deviceIdHex: String
    /// 1-based position in roster order; the default name is built from it.
    let position: Int
    let isThisDevice: Bool
    /// Holds the roster-signing role (§3). The badge says "Approves new
    /// devices" — the word "approving" is what the spec calls it, not what a
    /// family reads.
    let approves: Bool
    /// Whether Remove may be offered for this row from this install.
    let removable: Bool
}

/// Why Remove is not on offer, when it is not.
enum RemoveDeviceBlock: Equatable {
    /// §10.1: only the device holding the roster-signing role can sign the
    /// update. From anywhere else the tap has nothing to do but fail.
    case notTheApprovingDevice

    /// `core_revoke_devices_roster`: "the approving device cannot revoke
    /// itself; that takes the recovery material" (§14.2).
    case isTheApprovingDevice

    /// `core_revoke_devices_roster`: "a person must keep at least one device".
    case lastDevice
}

/// What the whole screen is showing, which decides the words above the list.
enum YourDevicesShape: Equatable {
    /// No roster at all: the overwhelming majority of installs, and not a
    /// failure. This person has one phone and has never linked another.
    case neverLinked

    /// A roster exists but names only this device.
    case onlyThisDevice

    /// Two or more devices.
    case several
}

/// Rows for a roster, in document order.
///
/// - Parameters:
///   - deviceIds: `coreRosterDeviceIds` output — active devices only, so a
///     removed device is absent rather than listed as gone. DL-4 keeps
///     tombstones in the document; a family list of "your devices" is about the
///     ones that are.
///   - ownDeviceId: this install's device id, or `nil` on an install that has
///     never linked (in which case no row is this device).
func ownDeviceRows(
    deviceIds: [Data],
    approvingDeviceId: Data,
    ownDeviceId: Data?
) -> [OwnDeviceRow] {
    let weApprove = ownDeviceId != nil && ownDeviceId == approvingDeviceId
    return deviceIds.enumerated().map { index, deviceId in
        let approves = deviceId == approvingDeviceId
        return OwnDeviceRow(
            deviceId: deviceId,
            deviceIdHex: deviceIdHex(deviceId),
            position: index + 1,
            isThisDevice: ownDeviceId != nil && deviceId == ownDeviceId,
            approves: approves,
            removable: weApprove && !approves && deviceIds.count > 1
        )
    }
}

/// Why `ownDeviceRows` withheld Remove from this row, or nil if it did not.
func removeBlockedReason(rows: [OwnDeviceRow], row: OwnDeviceRow) -> RemoveDeviceBlock? {
    if row.removable { return nil }
    if rows.count <= 1 { return .lastDevice }
    if row.approves { return .isTheApprovingDevice }
    // The three conditions above are exhaustive over what `ownDeviceRows`
    // actually withholds Remove for, so this last branch is the honest name for
    // the only case left: this install does not hold the signing role. Saying
    // `.lastDevice` here would have put "You cannot remove your only device"
    // under a row on a fleet of four.
    if !rows.contains(where: { $0.isThisDevice && $0.approves }) { return .notTheApprovingDevice }
    return .notTheApprovingDevice
}

/// Whether **Add a device** should be offered from this install at all.
///
/// §9.5 has the approving device sign the roster the new one is added to, so the
/// ceremony can only be completed from there. An install that is not it would
/// walk a person through a code, a camera and six digits and then fail at the
/// signature — so the entry is not offered, and the screen says which phone to
/// use instead.
///
/// An install with no roster is allowed: it is about to mint §3's genesis and
/// become device one, which is the overwhelmingly common case.
func canAddDevice(hasRoster: Bool, rows: [OwnDeviceRow]) -> Bool {
    if !hasRoster { return true }
    return rows.contains(where: { $0.isThisDevice && $0.approves })
}

/// The row a never-linked install shows for itself.
///
/// "Your devices" says "This is the only device signed in as you" and then, until
/// this existed, showed an empty list under it. There is nothing uncertain about
/// the claim — the person is signed in on this phone — so the screen shows the
/// phone it is talking about. No badge, because nothing approves anything yet,
/// and no Remove, because there is nothing else to be left with.
///
/// `ownDeviceId` is nil on an install that has never minted a device key, in
/// which case there is no code to show and the row is just the name.
func thisDeviceOnlyRow(ownDeviceId: Data?) -> OwnDeviceRow {
    OwnDeviceRow(
        deviceId: ownDeviceId ?? Data(),
        deviceIdHex: ownDeviceId.map(deviceIdHex) ?? "",
        position: 1,
        isThisDevice: true,
        approves: false,
        removable: false
    )
}

func yourDevicesShape(hasRoster: Bool, rows: [OwnDeviceRow]) -> YourDevicesShape {
    if !hasRoster { return .neverLinked }
    return rows.count <= 1 ? .onlyThisDevice : .several
}

/// The last four bytes of a device id, spaced, for a person telling two devices
/// apart when the names have not helped.
///
/// A device id is public (it is derived from a public key and rides in every
/// roster a contact holds), so showing part of one leaks nothing. Showing all of
/// it would be a wall of hex on a family screen; showing none of it would leave
/// two identically-named phones indistinguishable.
func shortDeviceCode(_ deviceIdHex: String) -> String {
    let tail = String(deviceIdHex.suffix(8))
    return stride(from: 0, to: tail.count, by: 4).map { offset -> String in
        let start = tail.index(tail.startIndex, offsetBy: offset)
        let end = tail.index(start, offsetBy: min(4, tail.count - offset))
        return String(tail[start..<end])
    }.joined(separator: " ")
}

/// A device id as hex. The same encoding `UserIdHex` gives a user id, named for
/// what it is holding — a device id is derived from a public key and is not a
/// user id, and the two must not be read as interchangeable at a call site.
func deviceIdHex(_ bytes: Data) -> String {
    UserIdHex.encode(bytes)
}
