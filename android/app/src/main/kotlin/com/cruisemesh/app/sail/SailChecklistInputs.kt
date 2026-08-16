package com.cruisemesh.app.sail

import android.content.Context
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.CoreSailChecklistInput
import uniffi.cruisemesh_core.CoreSailChecklistReport
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreSailChecklist

/**
 * What this phone can say about itself, in the shell's own terms, before any
 * of it is turned into checklist rows.
 *
 * Deliberately a plain data class of already-answered questions rather than a
 * bag of Android objects: gathering (which touches the database, prefs and the
 * permission system) and mapping are then separable, and the mapping is
 * testable without a device.
 */
data class SailChecklistDeviceState(
    /** Saved contacts. */
    val contactCount: Int,
    /** A Shore Pass is saved on this phone. */
    val shorePassConfigured: Boolean,
    /** The nearby-devices grant, without which nothing can be found close by. */
    val nearbyPermissionGranted: Boolean,
    /** Permission to post notifications. */
    val notificationsPermissionGranted: Boolean,
    /** Exempt from battery optimization, so it keeps going with the screen off. */
    val batteryOptimizationExempt: Boolean,
    /** A message has arrived over Bluetooth or the local network at least once. */
    val offlineDeliverySeen: Boolean,
    /** An encrypted backup has been saved at least once. */
    val backupCreated: Boolean,
)

/**
 * Gathers the "before you sail" checklist's inputs and hands them to the core.
 *
 * Every decision the checklist makes -- which steps there are, what order they
 * come in, which count as done, which are optional, whether the family is
 * ready to sail -- belongs to `core_sail_checklist`. This file only answers
 * questions about this phone and does the boring type mapping, so the two
 * platforms cannot drift into disagreeing about what "ready" means.
 */
object SailChecklistInputs {

    /**
     * Reads the phone's own facts. Touches the message store and shared
     * preferences, so callers run it off the main thread.
     *
     * The three permission answers are passed in rather than read here: the
     * screens that need them already track grants across the system dialogs
     * and lifecycle resumes, and a second, later reading of the same grant is
     * how a row ends up disagreeing with the banner above it.
     */
    fun deviceState(
        context: Context,
        store: MessageStore,
        nearbyPermissionGranted: Boolean,
        notificationsPermissionGranted: Boolean,
        batteryOptimizationExempt: Boolean,
    ): SailChecklistDeviceState = SailChecklistDeviceState(
        contactCount = store.listContacts().size,
        shorePassConfigured = RelayConfigStore.load(context) != null,
        nearbyPermissionGranted = nearbyPermissionGranted,
        notificationsPermissionGranted = notificationsPermissionGranted,
        batteryOptimizationExempt = batteryOptimizationExempt,
        offlineDeliverySeen = SailChecklistEvidence.hasSeenOfflineDelivery(context),
        backupCreated = SailChecklistEvidence.hasCreatedBackup(context),
    )

    /**
     * Maps this phone's facts onto the core's input record.
     *
     * `batteryOptimizationExempt` is never null on Android. The core takes it
     * as optional so iOS, which has no such setting, can leave the row out
     * entirely; passing null here would quietly drop a grant that really can
     * hold delivery back on this platform.
     */
    fun coreInput(state: SailChecklistDeviceState): CoreSailChecklistInput =
        CoreSailChecklistInput(
            contactCount = state.contactCount.coerceAtLeast(0).toULong(),
            shorePassConfigured = state.shorePassConfigured,
            bluetoothPermission = state.nearbyPermissionGranted,
            notificationsPermission = state.notificationsPermissionGranted,
            batteryOptimizationExempt = state.batteryOptimizationExempt,
            offlineDeliverySeen = state.offlineDeliverySeen,
            backupCreated = state.backupCreated,
        )

    /** [deviceState] straight through the core policy. Blocking; call off main. */
    fun report(
        context: Context,
        store: MessageStore,
        nearbyPermissionGranted: Boolean,
        notificationsPermissionGranted: Boolean,
        batteryOptimizationExempt: Boolean,
    ): CoreSailChecklistReport = coreSailChecklist(
        coreInput(
            deviceState(
                context,
                store,
                nearbyPermissionGranted,
                notificationsPermissionGranted,
                batteryOptimizationExempt,
            ),
        ),
    )
}
