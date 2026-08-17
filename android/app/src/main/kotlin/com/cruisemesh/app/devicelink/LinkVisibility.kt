package com.cruisemesh.app.devicelink

import android.util.Log
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreLinkGatedAction
import uniffi.cruisemesh_core.MessageStore

/**
 * §9.4's gate, for the one path core cannot see: this shell's own radios.
 *
 * A device between "the channel is confirmed" and "the roster head is
 * acknowledged" may not advertise, author, or ack ANYTHING. Core enforces that
 * for everything it holds -- authoring refuses, the ack planner names nothing,
 * the hint sets come back empty, carry offers nothing -- but core has never
 * heard of a BLE advertiser or an NSD registration, and a phone still shouting
 * its presence is not invisible whatever its store refuses to do.
 *
 * So the answer is cached here, refreshed from the core gate, and consulted by
 * [com.cruisemesh.app.mesh.MeshService] before it brings any radio up. One
 * listener, registered by that service for its lifetime, in the shape
 * `ChatViewEvents` and `RelaySyncEvents` already use.
 *
 * It fails closed. A store that cannot say whether this device is allowed to
 * speak is not a store this device should speak on the strength of -- and a
 * store that broken has nothing to send anyway.
 */
internal object LinkVisibility {
    private const val TAG = "LinkVisibility"

    @Volatile
    private var advertisingAllowed = true

    @Volatile
    private var listener: ((Boolean) -> Unit)? = null

    /**
     * What the mesh service has actually done, as opposed to what it has been
     * told. Guarded by [appliedLock] rather than volatile alone because
     * [awaitApplied] waits on it.
     */
    private val appliedLock = Object()
    private var applied = true

    /** Whether this device may make itself visible on the mesh at all. */
    fun mayAdvertise(): Boolean = advertisingAllowed

    /**
     * Re-read the core gate.
     *
     * Called by the mesh service (at start, and on its periodic tick, so an
     * activation that completed in another process is noticed) and by
     * [LinkDevSession] on each side of the pre-activation window.
     *
     * This only *asks* for the change. The mesh service's reaction is posted to
     * its main handler, so the radios are still up when this returns -- see
     * [awaitApplied] for the caller that must not proceed until they are not.
     */
    fun refresh(store: MessageStore) {
        val next = try {
            store.linkGate(CoreLinkGatedAction.ADVERTISE).allowed
        } catch (e: CoreException) {
            Log.w(TAG, "Could not read the device-link gate; staying quiet", e)
            false
        }
        if (next == advertisingAllowed) return
        advertisingAllowed = next
        val current = listener
        if (current == null) {
            // Nobody is holding radios, so there is nothing to wait for and a
            // waiter must not block on a service that is not running.
            markApplied(next)
        } else {
            current.invoke(next)
        }
    }

    /**
     * Block until the registered listener has actually applied [target].
     *
     * The one caller that needs this is the new device at the top of §9.4: the
     * silence has to be real before its first frame goes out, and "real" means
     * the BLE roles stopped and the LAN transport's NSD registration gone --
     * work that happens on the mesh service's main handler, one post later. A
     * device that sent its offer in that gap advertised during the very window
     * §9.4 exists to make silent.
     *
     * Returns false on timeout, which the caller must treat as a failure to go
     * quiet rather than as permission to continue.
     */
    fun awaitApplied(target: Boolean, timeoutMs: Long): Boolean {
        val deadline = System.currentTimeMillis() + timeoutMs
        synchronized(appliedLock) {
            while (applied != target) {
                val remaining = deadline - System.currentTimeMillis()
                if (remaining <= 0) return false
                try {
                    (appliedLock as Object).wait(remaining)
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    return false
                }
            }
        }
        return true
    }

    /** The mesh service reporting that the radios now match [allowed]. */
    fun markApplied(allowed: Boolean) {
        synchronized(appliedLock) {
            applied = allowed
            (appliedLock as Object).notifyAll()
        }
    }

    /** Register the mesh service's reaction. Fires once with the current answer. */
    fun register(onChange: (Boolean) -> Unit) {
        listener = onChange
        onChange(advertisingAllowed)
    }

    fun unregister() {
        listener = null
        // Nothing will apply anything from here, so release any waiter rather
        // than making it sit out its full timeout.
        markApplied(advertisingAllowed)
    }
}
