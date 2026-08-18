package com.cruisemesh.app.devicelink

import android.content.Context
import android.util.Log
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.identity.DeviceKeyStore
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreRevokeDevicesRoster

private const val TAG = "RemoveDeviceSession"

/** What a removal attempt ended as, in facts rather than copy. */
sealed interface RemoveDeviceResult {
    /**
     * The device is buried, the fleet's inbox key is rotated, and contacts have
     * been told (or the telling is queued).
     */
    data class Removed(
        val contactsTold: Int,
        val siblingsToHandOffTo: Int,
        val unresealableRecords: UInt,
    ) : RemoveDeviceResult

    /** Nothing was changed. [reason] is why, for the shell to word. */
    data class Refused(val reason: RemoveDeviceRefusal) : RemoveDeviceResult
}

/** Why a removal could not be attempted, or did not finish. */
enum class RemoveDeviceRefusal {
    /** No roster: this person has never linked a second device. */
    NO_DEVICES,

    /** §10.1: only the device holding the roster-signing role can sign this. */
    NOT_THE_APPROVING_DEVICE,

    /**
     * This device does not hold the inbox key the roster names, so it cannot
     * rotate it — a sibling that has not caught up with an earlier revocation.
     * Refusing is the whole point: rotating from a key this device cannot open
     * would strand the retained backlog.
     */
    INBOX_KEY_MISSING,

    /** This install has no device keys, so it is not part of any roster. */
    NO_DEVICE_KEYS,

    /**
     * An earlier removal was written down, its key reached storage, and it never
     * committed — so this device may have re-sealed part of its backlog to that
     * key already. Planning a fresh removal would mint different material at the
     * same generation and leave those rows unreadable forever, which is §10's
     * one forbidden outcome. See [RemoveDeviceSession.repairPending].
     */
    EARLIER_REMOVAL_UNFINISHED,

    /** Core refused the update or the commit. Details are in the log. */
    CORE_REFUSED,
}

/**
 * §10.1's "Remove device", end to end, on the device that may sign it.
 *
 * The order below is not this file's invention — `commit_own_revocation`
 * documents it and refuses to run out of order. What this class owes is the one
 * step core cannot take: making the rotated inbox key durable *between* the two
 * calls, so that a crash mid-ceremony leaves a fleet that can finish rather than
 * a backlog sealed to a secret that only ever existed in memory.
 *
 * 1. Ask core for the update ([coreRevokeDevicesRoster]) — the tombstone, the
 *    `seq + 1`, the re-signed certificates and the freshly minted key.
 * 2. `beginOwnRevocation` writes the journal row and hands over the key.
 * 3. [InboxKeyStore.save] makes it durable. **This is the load-bearing step.**
 * 4. `commitOwnRevocation` re-seals the backlog, adopts the roster, and points
 *    the inbound gate at it.
 * 5. [RosterGossipSender] tells the contacts — §10.1 step 4's surface for them
 *    is fed by the roster they now hold, not by a separate notice.
 *
 * # What it deliberately does not do
 *
 * It does not rotate the shared relay `family_token` (§10.2). That machinery has
 * no driver on either shell yet — no call site anywhere reaches
 * `begin_relay_rotation` — so claiming it here would be claiming a capability
 * that does not exist. The consequence is stated plainly in the confirm copy: a
 * removed phone stops being able to read the fleet's own sync traffic
 * immediately, and loses the relay mailbox when §10.2 ships.
 *
 * It also does not deliver [uniffi.cruisemesh_core.RevocationCommit.handoffs] to
 * siblings. Those ride self-sync, which has no shell transport yet either;
 * `revocationHandoffsFor` exists precisely so a sibling can be handed the
 * rotation whenever it is next reachable. The count is returned so the surface
 * can be honest that other devices catch up when they are next online.
 */
class RemoveDeviceSession(context: Context, private val identity: Identity) {
    private val appContext = context.applicationContext
    private val store: MessageStore = AppStore.get(appContext)

    /**
     * Settle whatever an interrupted ceremony left behind, before planning a new
     * one.
     *
     * Core states the whole decision procedure and this is it, unembellished: a
     * device that wakes to a pending revocation asks its own key store whether
     * it holds that generation. *No* means nothing was ever re-sealed to it, so
     * the journal row is worth nothing and is dropped. *Yes* means the backlog
     * may already be addressed to that key, and a fresh plan would mint
     * different material at the same generation — so this refuses rather than
     * guessing.
     *
     * Returns null when there is nothing in the way.
     */
    private fun repairPending(): RemoveDeviceRefusal? {
        val pending = try {
            store.pendingOwnRevocation()
        } catch (e: Exception) {
            Log.w(TAG, "could not read the unfinished removal", e)
            return RemoveDeviceRefusal.CORE_REFUSED
        } ?: return null

        val holdsIt = InboxKeyStore.generation(appContext) == pending.inboxKeyGeneration
        if (holdsIt) return RemoveDeviceRefusal.EARLIER_REMOVAL_UNFINISHED
        return try {
            store.abandonOwnRevocation()
            null
        } catch (e: Exception) {
            Log.w(TAG, "could not give up on an unfinished removal", e)
            RemoveDeviceRefusal.CORE_REFUSED
        }
    }

    fun remove(deviceId: ByteArray, nowMs: Long = System.currentTimeMillis()): RemoveDeviceResult {
        repairPending()?.let { return RemoveDeviceResult.Refused(it) }
        val roster = try {
            store.ownRoster()
        } catch (e: Exception) {
            Log.w(TAG, "could not read this person's device list", e)
            return RemoveDeviceResult.Refused(RemoveDeviceRefusal.CORE_REFUSED)
        } ?: return RemoveDeviceResult.Refused(RemoveDeviceRefusal.NO_DEVICES)

        val device = DeviceKeyStore.load(appContext)
            ?: return RemoveDeviceResult.Refused(RemoveDeviceRefusal.NO_DEVICE_KEYS)
        if (!device.deviceId.contentEquals(roster.approvingDeviceId)) {
            return RemoveDeviceResult.Refused(RemoveDeviceRefusal.NOT_THE_APPROVING_DEVICE)
        }
        val currentInboxKey = InboxKeyStore.current(appContext, identity, roster.inboxKeyGeneration)
            ?: return RemoveDeviceResult.Refused(RemoveDeviceRefusal.INBOX_KEY_MISSING)

        return try {
            val update = coreRevokeDevicesRoster(
                roster,
                identity.signPk,
                device.signSk,
                listOf(deviceId),
                currentInboxKey,
            )
            // (2) and (3): the key is durable before anything is re-sealed to it.
            val rotated = store.beginOwnRevocation(update, identity.signPk, device, nowMs)
            InboxKeyStore.save(appContext, rotated)

            val commit = store.commitOwnRevocation(
                update,
                identity.signPk,
                device,
                currentInboxKey,
                nowMs,
            )
            DeviceNameStore.forget(appContext, hex(deviceId))
            // §10.1 step 1's contact leg. Re-derived from the store rather than
            // read off `commit.contactUserIds`, so a commit that landed and then
            // crashed before sending is repaired by the next pass instead of
            // leaving contacts silently un-told.
            val told = RosterGossipSender.announceIfOwed(store, identity, nowMs)
            RemoveDeviceResult.Removed(
                contactsTold = told,
                siblingsToHandOffTo = commit.handoffs.size,
                unresealableRecords = commit.unresealableRecords,
            )
        } catch (e: Exception) {
            Log.w(TAG, "removing a device did not finish", e)
            RemoveDeviceResult.Refused(RemoveDeviceRefusal.CORE_REFUSED)
        }
    }

    private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }
}
