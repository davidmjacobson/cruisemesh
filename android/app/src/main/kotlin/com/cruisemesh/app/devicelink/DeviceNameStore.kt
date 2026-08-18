package com.cruisemesh.app.devicelink

import android.content.Context

/**
 * The name a person gave one of their own devices, and the day this phone first
 * saw it — both local to this install, deliberately.
 *
 * `specs/multi-device-v1.md` §13 WP6 asks the list for a name and an added
 * date, and the roster carries neither. A [uniffi.cruisemesh_core.DeviceCert]
 * has keys, flags, a signature and an `added_epoch` — which counts recovery
 * epochs, not days — and no display name at all, because §4's DL-5 keeps a
 * roster to public key material with nothing addressable or personal in it.
 *
 * So the words a family reads come from here instead: a nickname they typed on
 * this phone, and the first moment this phone saw that device id in its own
 * roster. Neither ever leaves the device. That is honest and it is small, and
 * the surface says so ("Names you give devices stay on this phone") rather than
 * implying the other phone knows what it has been called.
 *
 * Open for a future work package: a device name that travels would be a §8 sync
 * record, not a roster field — the roster stays free of anything DL-5 would
 * have to keep out of a contact's copy.
 */
object DeviceNameStore {
    private const val PREFS = "cruisemesh_device_names"
    private const val NAME_PREFIX = "name_"
    private const val FIRST_SEEN_PREFIX = "first_seen_"

    /** The name this person typed for `deviceIdHex`, or null if they have not. */
    fun name(context: Context, deviceIdHex: String): String? =
        prefs(context).getString(NAME_PREFIX + deviceIdHex, null)?.takeIf { it.isNotBlank() }

    fun setName(context: Context, deviceIdHex: String, name: String) {
        val trimmed = name.trim()
        prefs(context).edit().apply {
            if (trimmed.isEmpty()) remove(NAME_PREFIX + deviceIdHex) else putString(NAME_PREFIX + deviceIdHex, trimmed)
        }.apply()
    }

    /**
     * When this phone first saw `deviceIdHex`, recording `nowMs` the first time
     * and never moving it afterwards.
     *
     * First-seen rather than added-at, and named that way in the copy ("Seen
     * here since"), because a phone that joins a fleet of three learns about
     * two devices that were added long before it existed. Claiming those as
     * their added dates would be inventing a fact.
     */
    fun rememberSeen(context: Context, deviceIdHex: String, nowMs: Long): Long {
        val key = FIRST_SEEN_PREFIX + deviceIdHex
        val store = prefs(context)
        val existing = store.getLong(key, 0L)
        if (existing > 0L) return existing
        store.edit().putLong(key, nowMs).apply()
        return nowMs
    }

    /**
     * Stamp every device in a roster this phone has just adopted or applied.
     *
     * Called at adoption, never at render. Stamping while drawing a list meant
     * "first seen" was really "first looked at", so a device that had been in
     * the roster for a month dated from whenever somebody happened to open
     * Settings — and merely opening the screen wrote to disk. A device with no
     * stamp simply shows no date, which is the honest answer for one this phone
     * learned about before it kept notes.
     */
    fun rememberRoster(context: Context, deviceIdHexes: List<String>, nowMs: Long) {
        for (deviceIdHex in deviceIdHexes) rememberSeen(context, deviceIdHex, nowMs)
    }

    /** The recorded first sighting, or null if this phone has never seen it. */
    fun firstSeenMs(context: Context, deviceIdHex: String): Long? =
        prefs(context).getLong(FIRST_SEEN_PREFIX + deviceIdHex, 0L).takeIf { it > 0L }

    /** Forget a device the person removed. Nothing here outlives the roster. */
    fun forget(context: Context, deviceIdHex: String) {
        prefs(context).edit()
            .remove(NAME_PREFIX + deviceIdHex)
            .remove(FIRST_SEEN_PREFIX + deviceIdHex)
            .apply()
    }

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
