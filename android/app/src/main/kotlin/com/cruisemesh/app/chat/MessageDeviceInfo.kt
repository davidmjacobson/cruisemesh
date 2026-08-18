package com.cruisemesh.app.chat

import uniffi.cruisemesh_core.ContactDeviceState

/**
 * §8's receipt rule, at the one place it is allowed to be visible.
 *
 * `specs/multi-device-v1.md` §8: "delivered/read shown to contacts is
 * **any-device**; per-device receipt detail lives behind Advanced." The ticks in
 * a chat are therefore untouched — one tick, two ticks, blue ticks, exactly as
 * before, because a person messages a *person* and how many phones they carry is
 * a §2 non-goal to expose. What Advanced gets is the detail behind that: which
 * device of theirs a received message came from, and how many of their devices
 * a sent message is being delivered to.
 *
 * # An honest limit, stated once
 *
 * There is no per-device *receipt* in the core, and this file does not invent
 * one. Watermarks are person-and-chat keyed (`record_outgoing_receipt`), which
 * is precisely what makes "any-device" fall out for free once any sibling
 * records the receipt — and it means nothing anywhere knows that a message
 * reached their tablet but not their phone. So the sent-message line says how
 * many devices the message is addressed to and that a tick means any of them,
 * which is true, rather than a per-device table that would be fiction.
 *
 * Received messages are different: `StoredMessage.senderDeviceId` is a real
 * fact, recorded per row, and it is what the received line names.
 */
sealed interface DeviceInfoLine {
    /** "Sent from: their second device" — a received message's origin. */
    data class SentFrom(val label: DeviceLabel) : DeviceInfoLine

    /**
     * "Delivered to any of their N devices" — the honest shape of an
     * any-device receipt.
     */
    data class AddressedTo(val deviceCount: Int) : DeviceInfoLine

    /** The contact has never told us about their devices (every legacy peer). */
    data object NoDeviceDetail : DeviceInfoLine
}

/** How to name one device of a contact, without ever naming a person's hardware. */
sealed interface DeviceLabel {
    /** Their Nth device, 1-based, in the order their own list gives. */
    data class Numbered(val position: Int) : DeviceLabel

    /** A device they have since removed (DL-4: tombstoned, and stays so). */
    data object Removed : DeviceLabel

    /**
     * A device id that no list of theirs names — including every message from a
     * peer that has never sent one, which is every build in the field today.
     */
    data object Unknown : DeviceLabel
}

/**
 * Which of a contact's devices `senderDeviceId` is.
 *
 * @param activeDeviceIds `MessageStore.contactActiveDeviceIds` for this contact,
 *   in their roster's own order — so "their second device" means the same thing
 *   on every phone that holds the same list.
 * @param state `MessageStore.contactDeviceState` for the id, which is the only
 *   thing that can tell a device we have simply never heard of from one that was
 *   deliberately removed.
 */
fun deviceLabelFor(
    senderDeviceId: ByteArray?,
    activeDeviceIds: List<ByteArray>,
    state: ContactDeviceState,
): DeviceLabel {
    if (senderDeviceId == null || senderDeviceId.isEmpty()) return DeviceLabel.Unknown
    val index = activeDeviceIds.indexOfFirst { it.contentEquals(senderDeviceId) }
    return when {
        index >= 0 -> DeviceLabel.Numbered(index + 1)
        state == ContactDeviceState.REVOKED -> DeviceLabel.Removed
        else -> DeviceLabel.Unknown
    }
}

/**
 * The Advanced-only lines for one message, or an empty list when there is
 * nothing true to add.
 *
 * @param isOwn whether this person sent it.
 * @param label which of the contact's devices sent it, for a received message.
 * @param contactDeviceCount how many devices the contact currently has,
 *   0 for a contact who has never told us.
 */
fun messageDeviceInfoLines(
    isOwn: Boolean,
    label: DeviceLabel,
    contactDeviceCount: Int,
): List<DeviceInfoLine> = when {
    // A person with one device is the single-device world this whole spec
    // promises to keep invisible (§2 goal 1). Saying "delivered to any of their
    // 1 devices" would be leaking device count in the one case where there is
    // nothing to know.
    isOwn && contactDeviceCount > 1 -> listOf(DeviceInfoLine.AddressedTo(contactDeviceCount))
    isOwn -> emptyList()
    label is DeviceLabel.Unknown && contactDeviceCount == 0 ->
        listOf(DeviceInfoLine.NoDeviceDetail)
    label is DeviceLabel.Unknown -> emptyList()
    else -> listOf(DeviceInfoLine.SentFrom(label))
}
