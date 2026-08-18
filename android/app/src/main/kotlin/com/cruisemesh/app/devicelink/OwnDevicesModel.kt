package com.cruisemesh.app.devicelink

/**
 * "Your devices", as rows (`specs/multi-device-v1.md` §13 WP6).
 *
 * Everything here is arithmetic over facts the core already decided: which
 * device ids a roster names, which one holds the roster-signing role, and which
 * one this install is. No policy is invented — in particular the two refusals
 * below are core's, written down here only so the screen can decline to offer a
 * tap that `core_revoke_devices_roster` would refuse after the person had
 * already read a confirmation dialog.
 */
data class OwnDeviceRow(
    val deviceId: ByteArray,
    /** Full 32-character hex. Never shown whole — see [shortDeviceCode]. */
    val deviceIdHex: String,
    /** 1-based position in roster order; the default name is built from it. */
    val position: Int,
    val isThisDevice: Boolean,
    /**
     * Holds the roster-signing role (§3). The badge says "Approves new
     * devices" — the word "approving" is what the spec calls it, not what a
     * family reads.
     */
    val approves: Boolean,
    /** Whether Remove may be offered for this row from this install. */
    val removable: Boolean,
) {
    override fun equals(other: Any?): Boolean =
        other is OwnDeviceRow &&
            deviceId.contentEquals(other.deviceId) &&
            deviceIdHex == other.deviceIdHex &&
            position == other.position &&
            isThisDevice == other.isThisDevice &&
            approves == other.approves &&
            removable == other.removable

    override fun hashCode(): Int {
        var result = deviceIdHex.hashCode()
        result = 31 * result + position
        result = 31 * result + isThisDevice.hashCode()
        result = 31 * result + approves.hashCode()
        result = 31 * result + removable.hashCode()
        return result
    }
}

/** Why Remove is not on offer, when it is not. */
enum class RemoveDeviceBlock {
    /**
     * §10.1: only the device holding the roster-signing role can sign the
     * update. From anywhere else the tap has nothing to do but fail.
     */
    NOT_THE_APPROVING_DEVICE,

    /**
     * `core_revoke_devices_roster`: "the approving device cannot revoke
     * itself; that takes the recovery material" (§14.2).
     */
    IS_THE_APPROVING_DEVICE,

    /** `core_revoke_devices_roster`: "a person must keep at least one device". */
    LAST_DEVICE,
}

/**
 * What the whole screen is showing, which decides the words above the list.
 */
enum class YourDevicesShape {
    /**
     * No roster at all: the overwhelming majority of installs, and not a
     * failure. This person has one phone and has never linked another.
     */
    NEVER_LINKED,

    /** A roster exists but names only this device. */
    ONLY_THIS_DEVICE,

    /** Two or more devices. */
    SEVERAL,
}

/**
 * Rows for a roster, in document order.
 *
 * @param deviceIds `core_roster_device_ids` output — active devices only, so a
 *   removed device is absent rather than listed as gone. DL-4 keeps tombstones
 *   in the document; a family list of "your devices" is about the ones that are.
 * @param ownDeviceId this install's device id, or `null` on an install that has
 *   never linked (in which case no row is this device).
 */
fun ownDeviceRows(
    deviceIds: List<ByteArray>,
    approvingDeviceId: ByteArray,
    ownDeviceId: ByteArray?,
): List<OwnDeviceRow> {
    val weApprove = ownDeviceId != null && ownDeviceId.contentEquals(approvingDeviceId)
    return deviceIds.mapIndexed { index, deviceId ->
        val approves = deviceId.contentEquals(approvingDeviceId)
        OwnDeviceRow(
            deviceId = deviceId,
            deviceIdHex = hexOf(deviceId),
            position = index + 1,
            isThisDevice = ownDeviceId != null && deviceId.contentEquals(ownDeviceId),
            approves = approves,
            removable = weApprove && !approves && deviceIds.size > 1,
        )
    }
}

/** Why [ownDeviceRows] withheld Remove from this row, or null if it did not. */
fun removeBlockedReason(
    rows: List<OwnDeviceRow>,
    row: OwnDeviceRow,
): RemoveDeviceBlock? = when {
    row.removable -> null
    rows.size <= 1 -> RemoveDeviceBlock.LAST_DEVICE
    row.approves -> RemoveDeviceBlock.IS_THE_APPROVING_DEVICE
    // The three conditions above are exhaustive over what `ownDeviceRows`
    // actually withholds Remove for, so this last branch is the honest name for
    // the only case left: this install does not hold the signing role. Saying
    // LAST_DEVICE here would have put "You cannot remove your only device"
    // under a row on a fleet of four.
    else -> RemoveDeviceBlock.NOT_THE_APPROVING_DEVICE
}

/**
 * Whether **Add a device** should be offered from this install at all.
 *
 * §9.5 has the approving device sign the roster the new one is added to, so the
 * ceremony can only be completed from there. An install that is not it would
 * walk a person through a code, a camera and six digits and then fail at the
 * signature — so the button is not offered, and the screen says which phone to
 * use instead.
 *
 * An install with no roster is allowed: it is about to mint §3's genesis and
 * become device one, which is the overwhelmingly common case.
 */
fun canAddDevice(hasRoster: Boolean, rows: List<OwnDeviceRow>): Boolean =
    !hasRoster || rows.any { it.isThisDevice && it.approves }

/**
 * The row a never-linked install shows for itself.
 *
 * "Your devices" says "This is the only device signed in as you" and then, until
 * this existed, showed an empty list under it. There is nothing uncertain about
 * the claim — the person is signed in on this phone — so the screen shows the
 * phone it is talking about. No badge, because nothing approves anything yet,
 * and no Remove, because there is nothing else to be left with.
 *
 * `ownDeviceId` is null on an install that has never minted a device key, in
 * which case there is no code to show and the row is just the name.
 */
fun thisDeviceOnlyRow(ownDeviceId: ByteArray?): OwnDeviceRow = OwnDeviceRow(
    deviceId = ownDeviceId ?: ByteArray(0),
    deviceIdHex = ownDeviceId?.let(::hexOf).orEmpty(),
    position = 1,
    isThisDevice = true,
    approves = false,
    removable = false,
)

fun yourDevicesShape(hasRoster: Boolean, rows: List<OwnDeviceRow>): YourDevicesShape = when {
    !hasRoster -> YourDevicesShape.NEVER_LINKED
    rows.size <= 1 -> YourDevicesShape.ONLY_THIS_DEVICE
    else -> YourDevicesShape.SEVERAL
}

/**
 * The last four bytes of a device id, spaced, for a person telling two devices
 * apart when the names have not helped.
 *
 * A device id is public (it is derived from a public key and rides in every
 * roster a contact holds), so showing part of one leaks nothing. Showing all of
 * it would be a wall of hex on a family screen; showing none of it would leave
 * two identically-named phones indistinguishable.
 */
fun shortDeviceCode(deviceIdHex: String): String =
    deviceIdHex.takeLast(8).chunked(4).joinToString(" ")

private fun hexOf(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }
