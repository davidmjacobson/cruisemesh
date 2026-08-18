package com.cruisemesh.app.devicelink

import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.cruisemesh_core.CoreLinkActivationStage
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.MessageStore

/**
 * §10 step 5, seen from the phone that was removed.
 *
 * Core is where the decision lives: a signed roster of this person's own devices
 * that buries this one ejects it — the roster is stored, the fleet projection is
 * cleared, and the activation stage becomes
 * [CoreLinkActivationStage.REVOKED], which refuses advertising, authoring and
 * acking alike. This object is the one thing core cannot do from there: tell the
 * screens.
 *
 * It reads the stage rather than remembering an event, so a device that was
 * ejected in a previous process still knows on the next launch. [markRemoved]
 * exists only so the mesh service can flip the surface in the same breath as
 * applying a notice, instead of leaving the person looking at a chat list until
 * something re-reads the store.
 *
 * # Which way it fails
 *
 * The opposite way to [LinkVisibility], deliberately. That one fails closed
 * toward silence, because a store it cannot read is not one to shout on the
 * strength of. This one keeps its last answer on a read it cannot make, because
 * the cost of guessing wrong here is telling a person their phone was removed
 * when it was not — and the radios are gated by core's own answer regardless, so
 * a wrong guess here never puts a removed device back on the air.
 */
internal object DeviceRemovalStatus {
    private const val TAG = "DeviceRemovalStatus"

    private val _removed = MutableStateFlow(false)

    /** Whether this device's person has removed it from their devices. */
    val removed: StateFlow<Boolean> = _removed.asStateFlow()

    /** Re-read the stage. Cheap: one select against a single-row table. */
    fun refresh(store: MessageStore) {
        _removed.value = try {
            store.linkActivation().stage == CoreLinkActivationStage.REVOKED
        } catch (e: CoreException) {
            Log.w(TAG, "Could not read this device's link stage; keeping the last answer", e)
            _removed.value
        }
    }

    /** A notice this device just applied said it was the one being buried. */
    fun markRemoved() {
        _removed.value = true
    }
}
