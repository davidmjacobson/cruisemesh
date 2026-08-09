package com.cruisemesh.app.mesh

import android.util.Log
import com.cruisemesh.app.relay.CoreRelayDriver
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayHttpException
import com.cruisemesh.app.relay.RelayPassEngine
import com.cruisemesh.app.relay.relayShadowPermitted
import uniffi.cruisemesh_core.CoreRelayContactConfig
import uniffi.cruisemesh_core.CoreRelayEndpointConfig
import uniffi.cruisemesh_core.CoreRelayShadowCapture
import uniffi.cruisemesh_core.CoreRelayShadowLane
import uniffi.cruisemesh_core.CoreRelayShadowSampler
import uniffi.cruisemesh_core.CoreRelayShadowStep
import uniffi.cruisemesh_core.CoreRelayTransportError
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreRelayShadowCompare
import uniffi.cruisemesh_core.coreRelayShadowSample

private const val TAG = "MeshService"

/**
 * The migration canary: on a few legacy passes a day, ask what the core engine
 * would have done with exactly what the legacy engine saw, and record where
 * the two disagree.
 *
 * # The three hard rules, and how each is kept
 *
 * **It performs no network I/O of its own.** Not by discipline -- by the types
 * it is built from. Look at what this class can reach: a [MessageStore], a
 * function returning the current engine, and [RelayShadowPassCapture], whose
 * every field is a byte array, a number, a string or an enum. There is no
 * `Network`, no `HttpURLConnection`, no `RelayClient`, no URL and no function
 * handle anywhere in the capture, so there is nothing here that *could* be
 * asked to open a connection. The comparison itself is a pure core function
 * over those values. `RelayShadowAdapterTest` asserts that shape by
 * reflection, so a field of a networking type cannot be added quietly.
 *
 * **It writes nothing to the production store.** The one store call it makes
 * is [MessageStore.noteRelayShadowReport], which touches the bounded
 * diagnostics ring and nothing else, and which takes a report that cannot be
 * turned back into a row -- counts and enums only. There is no code path from
 * a capture to a marker, a cursor, a receipt or a health record.
 *
 * **It cannot run against the core engine.** [relayShadowPermitted] is the
 * gate. Comparing the core planner against the core engine would agree every
 * time while looking exactly like evidence, which is worse than no canary.
 *
 * # What a sample costs
 *
 * A bounded number of samples a day, spaced apart, decided by core so both
 * shells mean the same thing by "bounded" ([coreRelayShadowSample]). A sample
 * holds at most [uniffi.cruisemesh_core.coreRelayShadowMaxRows] rows and drops
 * the rest, because a sealed payload can be half a megabyte and a capture is
 * held whole while it is compared. Nothing is retained after the comparison.
 *
 * # Its lifetime
 *
 * It is deleted with the legacy engine. This is scaffolding that exists to
 * earn the evidence for that deletion, not an architecture.
 */
internal class RelayShadowAdapter(
    private val store: MessageStore,
    private val passEngine: () -> RelayPassEngine,
    private val shadowEnabled: () -> Boolean,
) {

    /** Sampling state between passes. Nothing about correctness depends on it surviving a restart. */
    private var sampler = CoreRelayShadowSampler(0L, 0u, 0L)

    /**
     * Begin capturing this pass, or return null when it is not a sampled one.
     *
     * Null is the overwhelmingly common answer, and it is what makes the
     * canary free: an unsampled pass allocates nothing and the legacy engine's
     * upload loops hand their observations to a null reference that ignores
     * them.
     */
    @Synchronized
    fun beginPass(nowMs: Long): RelayShadowPassCapture? {
        if (!relayShadowPermitted(passEngine(), shadowEnabled())) return null
        val decision = coreRelayShadowSample(sampler, nowMs)
        sampler = decision.next
        return if (decision.sample) RelayShadowPassCapture() else null
    }

    /**
     * Compare what was captured and record what was found.
     *
     * Called after the legacy pass has finished its uploads, so nothing this
     * returns can change what that pass did -- which is the other half of "no
     * second writer": even a comparison that found a disagreement has no way
     * to act on it.
     */
    fun finishPass(
        capture: RelayShadowPassCapture?,
        own: RelayConfig?,
        contacts: List<CoreRelayShadowContact>,
        nowMs: Long,
    ) {
        if (capture == null) return
        val report = try {
            coreRelayShadowCompare(
                CoreRelayShadowCapture(
                    own = own?.let { CoreRelayEndpointConfig(it.relayUrl, it.relayToken) },
                    contacts = contacts.map {
                        CoreRelayContactConfig(it.userId, it.relayUrl, it.relayToken, it.endpointUsable)
                    },
                    steps = capture.steps(),
                    skippedRecipients = capture.skippedRecipients(),
                    rowsUnshadowed = capture.rowsUnshadowed(),
                ),
            )
        } catch (e: Exception) {
            // A canary must never be the reason a pass reports a failure. It
            // has already had no effect on anything the pass did.
            Log.w(TAG, "Relay shadow comparison failed: ${e.message}")
            return
        }
        store.noteRelayShadowReport(report, nowMs)
        if (report.mismatches.isNotEmpty()) {
            Log.w(
                TAG,
                "Relay shadow found ${report.mismatches.size} divergence(s) over " +
                    "${report.stepsCompared} row(s); see the protocol event ring",
            )
        }
    }
}

/**
 * One contact, as the canary needs to see them: the card fields a destination
 * is resolved from, plus whether this device is still willing to use that
 * card's endpoint.
 *
 * A value type on purpose. Handing the canary the shell's own `Contact` would
 * hand it a store row; this carries only what a routing decision reads.
 */
internal data class CoreRelayShadowContact(
    val userId: ByteArray,
    val relayUrl: String?,
    val relayToken: String?,
    val endpointUsable: Boolean,
) {
    override fun equals(other: Any?): Boolean =
        other is CoreRelayShadowContact &&
            userId.contentEquals(other.userId) &&
            relayUrl == other.relayUrl &&
            relayToken == other.relayToken &&
            endpointUsable == other.endpointUsable

    override fun hashCode(): Int = userId.contentHashCode()
}

/**
 * What one sampled legacy pass remembers about its receipt and authored
 * uploads.
 *
 * Every method here takes values and returns nothing. It is not given the
 * relay config it is posting to as a live object, not given the connection,
 * not given a callback -- only what was observed, after it was observed.
 */
internal class RelayShadowPassCapture {

    private val steps = mutableListOf<CoreRelayShadowStep>()
    private val skipped = mutableListOf<ByteArray>()
    private var unshadowed = 0
    private var dropped = 0

    /** A row the legacy engine posted and the relay accepted. */
    fun noteSucceeded(
        lane: CoreRelayShadowLane,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        recipientUserId: ByteArray,
        sealed: ByteArray,
        expiryMs: Long,
        endpoint: RelayConfig,
    ) = record(
        lane, msgId, hopTtl, recipientHint, recipientUserId, sealed, expiryMs,
        endpoint = endpoint,
        status = 200,
        relayCode = null,
        transportError = null,
        markedPosted = true,
        continuedLane = true,
    )

    /**
     * A row the legacy engine posted and the relay or the link refused.
     *
     * @param continuedLane whether it went on to offer the next row of this
     *   lane to the same mailbox, which is the axis a status code cannot
     *   answer and the core engine decides differently.
     */
    fun noteFailed(
        lane: CoreRelayShadowLane,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        recipientUserId: ByteArray,
        sealed: ByteArray,
        expiryMs: Long,
        endpoint: RelayConfig,
        error: Exception,
        continuedLane: Boolean,
    ) {
        // A family rate limit is wrapped as it unwinds the upload loops, so
        // the relay's own answer is one cause down.
        val http = error as? RelayHttpException ?: error.cause as? RelayHttpException
        record(
            lane, msgId, hopTtl, recipientHint, recipientUserId, sealed, expiryMs,
            endpoint = endpoint,
            status = http?.code ?: 0,
            relayCode = http?.relayCode,
            transportError = if (http != null) {
                null
            } else {
                CoreRelayDriver.classify(error)
            },
            markedPosted = false,
            continuedLane = continuedLane,
        )
    }

    /** A row the legacy engine declined to post at all, having resolved no mailbox for it. */
    fun noteDeclined(
        lane: CoreRelayShadowLane,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        recipientUserId: ByteArray,
        sealed: ByteArray,
        expiryMs: Long,
    ) = record(
        lane, msgId, hopTtl, recipientHint, recipientUserId, sealed, expiryMs,
        endpoint = null,
        status = 0,
        relayCode = null,
        transportError = null,
        markedPosted = false,
        continuedLane = true,
    )

    @Suppress("LongParameterList")
    private fun record(
        lane: CoreRelayShadowLane,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        recipientUserId: ByteArray,
        sealed: ByteArray,
        expiryMs: Long,
        endpoint: RelayConfig?,
        status: Int,
        relayCode: String?,
        transportError: CoreRelayTransportError?,
        markedPosted: Boolean,
        continuedLane: Boolean,
    ) {
        if (steps.size >= MAX_ROWS) {
            // Counted rather than silently forgotten: a report that compared
            // sixteen of forty rows must not read as a report about forty.
            dropped++
            unshadowed++
            return
        }
        steps += CoreRelayShadowStep(
            lane = lane,
            msgId = msgId,
            hopTtl = hopTtl,
            recipientHint = recipientHint,
            recipientUserId = recipientUserId,
            sealed = sealed,
            expiryMs = expiryMs,
            legacyEndpoint = endpoint?.let { CoreRelayEndpointConfig(it.relayUrl, it.relayToken) },
            status = status.coerceIn(0, UShort.MAX_VALUE.toInt()).toUShort(),
            relayCode = relayCode,
            transportError = transportError,
            legacyMarkedPosted = markedPosted,
            legacyContinuedLane = continuedLane,
        )
    }

    /** Recipients the legacy engine excluded from its queue query before selecting anything. */
    fun noteSkippedRecipients(recipients: List<ByteArray>) {
        skipped += recipients
    }

    /**
     * Rows this capture deliberately cannot speak for: group fan-out rows,
     * which core's upload lanes do not decompose, and carried rows, which a
     * later package owns.
     */
    fun noteUnshadowed(rows: Int) {
        if (rows > 0) unshadowed += rows
    }

    internal fun steps(): List<CoreRelayShadowStep> = steps.toList()

    internal fun skippedRecipients(): List<ByteArray> = skipped.toList()

    internal fun rowsUnshadowed(): UInt = unshadowed.toUInt()

    internal fun rowsDropped(): Int = dropped

    private companion object {
        /**
         * Mirrors `RELAY_SHADOW_MAX_ROWS`, read through the binding so the
         * number is core's rather than this file's.
         */
        val MAX_ROWS = uniffi.cruisemesh_core.coreRelayShadowMaxRows().toInt()
    }
}
