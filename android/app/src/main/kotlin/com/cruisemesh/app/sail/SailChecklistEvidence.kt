package com.cruisemesh.app.sail

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.cruisemesh_core.PeerConnectionTransport
import uniffi.cruisemesh_core.corePeerTransportForArrival

/**
 * The two facts the "before you sail" checklist needs that nothing else on
 * this phone already remembers: that a message has arrived without the
 * internet, and that a backup has been made.
 *
 * Both are one-way latches. "Ever", not "recently": once a family has watched
 * the phones talk with the internet off, a quiet afternoon does not take that
 * back, and a checklist that un-ticks itself would send people round the same
 * loop before every trip. Nothing here clears.
 *
 * Which arrivals count is the core's decision, not this file's -- see
 * [recordArrival].
 */
object SailChecklistEvidence {
    private const val PREFS = "cruisemesh_sail_checklist"
    private const val KEY_OFFLINE_DELIVERY_SEEN = "offline_delivery_seen"
    private const val KEY_BACKUP_CREATED = "backup_created"

    // Lets the delivery path skip a redundant write once the latch is known to
    // be set. It only ever shortcuts *writing*: readers always ask the prefs,
    // so a stale `true` here can never invent a proof that was never stored.
    @Volatile
    private var offlineDeliveryLatched = false

    private val changeCount = MutableStateFlow(0)

    /**
     * Bumps whenever a latch is newly set, so the home card can recompute at
     * the exact moment the airplane-mode test succeeds -- the one moment the
     * family is watching it -- instead of waiting for a resume.
     */
    val changes: StateFlow<Int> = changeCount.asStateFlow()

    /** True once any message has arrived over Bluetooth or the local network. */
    fun hasSeenOfflineDelivery(context: Context): Boolean =
        prefs(context).getBoolean(KEY_OFFLINE_DELIVERY_SEEN, false)

    /**
     * Files an arrival's transport code (`MessageArrival.transport`).
     *
     * The classification is the core's, so the shell never re-decides which
     * codes mean "no internet was involved". Everything except the Shore Pass
     * path counts, carried arrivals included: a message another phone muled
     * over Bluetooth still made its last hop with the internet out, which is
     * exactly what the step asks a family to see for themselves.
     *
     * Best-effort by design -- a checklist tick is never worth failing a real
     * delivery over, so callers run this after the message is durably stored.
     */
    fun recordArrival(context: Context, transport: UByte) {
        if (offlineDeliveryLatched) return
        if (corePeerTransportForArrival(transport) == PeerConnectionTransport.SHORE_PASS) return
        prefs(context).edit().putBoolean(KEY_OFFLINE_DELIVERY_SEEN, true).apply()
        offlineDeliveryLatched = true
        changeCount.value += 1
    }

    /** True once an encrypted backup file has been written on this phone. */
    fun hasCreatedBackup(context: Context): Boolean =
        prefs(context).getBoolean(KEY_BACKUP_CREATED, false)

    /**
     * A backup file was saved. Written after the bytes reach the chosen
     * destination, not when they are encoded: the step is about a copy that
     * exists somewhere, and a save the user cancelled leaves nothing behind.
     */
    fun markBackupCreated(context: Context) {
        prefs(context).edit().putBoolean(KEY_BACKUP_CREATED, true).apply()
        changeCount.value += 1
    }

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
