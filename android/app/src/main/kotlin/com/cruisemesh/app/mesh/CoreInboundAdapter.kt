package com.cruisemesh.app.mesh

import android.util.Log
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreInboundSource
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.SeenIds
import uniffi.cruisemesh_core.encodeEnvelopeFrame

private const val TAG = "MeshService"

/**
 * The driver half of the inbound path: hand one §6.4 envelope to the single
 * core transaction, then execute the bounded work it hands back.
 *
 * What core decides here, and this class does not: whether the frame parses,
 * whether it is a duplicate, whether it has expired or carries refusable
 * header fields, whether this device can open it (pairwise or as a group
 * member), whether a carried row is enqueued and how it is classified, whether
 * a hop-decremented copy should be flooded onward, and which
 * [CoreInboundDisposition] the relay ack rule may act on. All of that used to
 * be re-derived here in Kotlin beside a second copy in Swift and a third in the
 * simulator; it now lives once in `core/src/session/mesh_receive.rs`.
 *
 * What this class keeps is what a driver is for: the thread it runs on, the
 * transports the flood copy goes out over, and the durable native delivery of
 * an opened payload -- the per-kind handlers, chat rows, notifications and
 * receipts that are presentation, not disposition.
 *
 * The DTN D4 ordering is the whole point of the split and is preserved exactly:
 * core defers the flood-dedupe `seen` record and the ACK-01 consumed-hidden
 * evidence for a *delivered* payload into a [uniffi.cruisemesh_core.CoreInboundCommit]
 * token, and this class applies that token only after [Intents.deliver]
 * reports the durable delivery landed. A delivery that fails drops the token
 * unused: the `msg_id` stays unrecorded and re-presentable, and the reported
 * disposition is [CoreInboundDisposition.FAILED], which is never acked.
 */
internal class CoreInboundAdapter(
    private val store: MessageStore,
    private val seenIds: SeenIds,
    private val intents: Intents,
    private val now: () -> Long = System::currentTimeMillis,
) {

    /**
     * The typed actions core hands back, executed by the shell. Every argument
     * is a plain value; nothing here infers a retry, ack, carry or disposition
     * policy of its own.
     */
    interface Intents {
        /**
         * Flood [frame] -- already hop-decremented and encoded by core -- to
         * every live link except the one it arrived on. [sourceAddress] is
         * `null` for a relay-sourced envelope, which has no arrival link to
         * exclude.
         */
        fun flood(sourceAddress: String?, frame: ByteArray)

        /**
         * Durably apply one opened payload: decode the body, run the per-kind
         * handler, write the chat row, author receipts, notify.
         *
         * [groupId] is core's statement of which lane this payload belongs to
         * -- non-null for a group body whose signer and this device are both
         * members, null for a pairwise body. Returns `false` when the durable
         * persist failed, which is the one signal that must stop the commit
         * token from being applied.
         */
        fun deliver(
            sourceAddress: String?,
            hopTtl: UByte,
            msgId: ByteArray,
            senderUserId: ByteArray,
            groupId: ByteArray?,
            payload: ByteArray,
            identity: Identity,
        ): Boolean

        /**
         * A carried row was enqueued and core classified it *family* -- bound
         * for someone this device knows. The relay upload lane is nudged so an
         * internet-connected phone can proxy it onward.
         */
        fun onFamilyCarry()
    }

    /**
     * Runs [envelope] through the core transaction and executes the result.
     *
     * The caller owns the concurrency claim around this call (see
     * [InboundEnvelopeAdmission]): core records `seen` itself, inside this
     * call, so holding the claim across it keeps the existing guarantee that no
     * other thread can re-claim a `msg_id` between the record landing and the
     * claim releasing.
     */
    fun process(
        sourceAddress: String?,
        envelope: Frame.Envelope,
        identity: Identity,
    ): CoreInboundDisposition {
        val sourceLabel = sourceAddress ?: "relay"
        val source = if (sourceAddress == null) CoreInboundSource.RELAY else CoreInboundSource.MESH
        val frame = encodeEnvelopeFrame(
            envelope.msgId,
            envelope.hopTtl,
            envelope.expiry,
            envelope.recipientHint,
            envelope.sealed,
        )
        val outcome = try {
            store.processInboundFrame(identity, seenIds, source, frame, now())
        } catch (e: CoreException) {
            // A store failure inside the transaction leaves the msg_id
            // unrecorded by construction, so the next copy re-dispatches and
            // the relay copy is never acked.
            Log.w(TAG, "Deferring envelope from $sourceLabel: inbound transaction failed (${e.message})")
            return CoreInboundDisposition.FAILED
        }

        // Flood and carry first, before the local delivery that may fail --
        // the same order the iOS driver uses. Core has already committed the
        // carry row and decided the hop-decremented copy exists; both are for
        // the *other* recipients of this envelope and have nothing to do with
        // whether this device's own copy persisted. Running them after the
        // delivery meant a full disk silently dropped everyone else's copy on
        // the floor, and the retry that follows a FAILED cannot bring it back:
        // the next re-presentation dedupes or re-fails the same way.
        outcome.relayFrame?.let { intents.flood(sourceAddress, it) }
        if (outcome.carriedFamily) {
            intents.onFamilyCarry()
        }

        val payload = outcome.deliveredPayloads.firstOrNull()
        if (payload != null) {
            val sender = outcome.deliveredSender
            val commit = outcome.commit
            if (sender == null || commit == null) {
                // Core states both alongside every delivered payload. Treating
                // a missing one as a failed delivery keeps the envelope
                // re-presentable rather than acking something undelivered.
                Log.w(TAG, "Deferring envelope from $sourceLabel: delivered payload had no sender or commit token")
                return CoreInboundDisposition.FAILED
            }
            val delivered = intents.deliver(
                sourceAddress,
                envelope.hopTtl,
                envelope.msgId,
                sender,
                outcome.deliveredGroupId,
                payload,
                identity,
            )
            if (!delivered) {
                Log.w(TAG, "Deferring envelope from $sourceLabel: durable delivery failed")
                return CoreInboundDisposition.FAILED
            }
            // DTN D4: the deferred `seen` record and the ACK-01 hidden-kind
            // evidence land only now, after the delivery this device owns
            // actually persisted.
            store.coreCommitInboundDelivery(seenIds, commit)
        }

        return outcome.disposition
    }
}
