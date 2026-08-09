package com.cruisemesh.app.mesh

import android.util.Log
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayHttpException
import com.cruisemesh.app.relay.RelayPassEngine
import com.cruisemesh.app.relay.relayClassifyTransportError
import com.cruisemesh.app.relay.relayShadowPermitted
import uniffi.cruisemesh_core.CoreRelayContactConfig
import uniffi.cruisemesh_core.CoreRelayEndpointConfig
import uniffi.cruisemesh_core.CoreRelayShadowCapture
import uniffi.cruisemesh_core.CoreRelayShadowLane
import uniffi.cruisemesh_core.CoreRelayShadowReport
import uniffi.cruisemesh_core.CoreRelayShadowSampler
import uniffi.cruisemesh_core.CoreRelayShadowStep
import uniffi.cruisemesh_core.CoreRelayTransportError
import uniffi.cruisemesh_core.coreRelayShadowCompare
import uniffi.cruisemesh_core.coreRelayShadowSample

private const val TAG = "MeshService"

/**
 * The one thing the canary is allowed to write.
 *
 * A one-method interface rather than the message store, and that is the
 * difference between "the shadow does not write" as an intention and as a
 * property. `MessageStore` is the single object carrying
 * `markOutboundEnvelopeRelayPosted`, `noteContactRelayRejected`,
 * `ingestRelayPage` and every cursor writer as public methods: an adapter
 * holding one has every operational write in the app within reach of a single
 * line, and only a reviewer's attention stands between the two engines and a
 * second writer. An adapter holding *this* cannot express one.
 *
 * `MeshService` constructs it as `store::noteRelayShadowReport`, which touches
 * the bounded diagnostics ring and nothing else.
 */
internal fun interface RelayShadowReportSink {
    fun note(report: CoreRelayShadowReport, nowMs: Long)
}

/**
 * The migration canary: on a few legacy passes a day, ask what the core engine
 * would have done with exactly what the legacy engine saw, and record where
 * the two disagree.
 *
 * # The three hard rules, and how each is kept
 *
 * **It performs no network I/O of its own.** Not by discipline -- by the types
 * it is built from. Look at what this class can reach: a
 * [RelayShadowReportSink], three functions returning plain values, and
 * [RelayShadowPassCapture], whose every field is a byte array, a number, a
 * string or an enum. There is no `Network`, no `HttpURLConnection`, no
 * `RelayClient`, no driver, no URL and no function handle anywhere in the
 * capture, so there is nothing here that *could* be asked to open a
 * connection. The comparison itself is a pure core function over those values.
 * `RelayShadowAdapterTest` asserts that shape by reflection, so a field of a
 * networking type cannot be added quietly.
 *
 * **It writes nothing to the production store.** It does not hold the store.
 * The one write it can reach is [RelayShadowReportSink.note], which touches
 * the bounded diagnostics ring and takes a report that cannot be turned back
 * into a row -- counts and enums only. There is no code path from a capture to
 * a marker, a cursor, a receipt or a health record, because there is no object
 * here that has one.
 *
 * **It cannot run against the core engine.** [relayShadowPermitted] is the
 * gate. Comparing the core planner against the core engine would agree every
 * time while looking exactly like evidence, which is worse than no canary.
 *
 * # What a sample costs
 *
 * A bounded number of samples a day, spaced apart, decided by core so both
 * shells mean the same thing by "bounded" ([coreRelayShadowSample]) and
 * persisted, so the bound is per day rather than per process launch -- a
 * service Android restarts under memory pressure would otherwise be guaranteed
 * a sample every launch, and launches are not bounded by anything.
 *
 * The sample itself is spent on the first row worth comparing, not at the top
 * of the pass. The common relay pass is a poll tick with an empty outbound and
 * receipt queue, and a day of samples spent on those yields a day of reports
 * that compared nothing. A capture with no rows is discarded and costs no
 * sample.
 *
 * A sample holds at most [uniffi.cruisemesh_core.coreRelayShadowMaxRows] rows
 * and drops the rest, and holds no payloads at all -- a row is captured as the
 * *length* of its sealed body, which is the whole of what "could core have
 * formed this request" turns on. Nothing is retained after the comparison.
 *
 * # Its lifetime
 *
 * It is deleted with the legacy engine. This is scaffolding that exists to
 * earn the evidence for that deletion, not an architecture.
 */
internal class RelayShadowAdapter(
    private val sink: RelayShadowReportSink,
    private val passEngine: () -> RelayPassEngine,
    private val shadowEnabled: () -> Boolean,
    private val loadSampler: () -> CoreRelayShadowSampler,
    private val saveSampler: (CoreRelayShadowSampler) -> Unit,
) {

    /**
     * Begin capturing this pass, or return null when the canary may not run at
     * all.
     *
     * Cheap either way: this decides only whether capturing is *permitted*.
     * Whether this pass is one of the sampled ones is decided the first time
     * there is a row to compare, so a pass that turns out to have none spends
     * nothing and leaves the day's budget for a pass that carries evidence.
     */
    fun beginPass(nowMs: Long): RelayShadowPassCapture? {
        if (!relayShadowPermitted(passEngine(), shadowEnabled())) return null
        return RelayShadowPassCapture { armSample(nowMs) }
    }

    @Synchronized
    private fun armSample(nowMs: Long): Boolean {
        val decision = coreRelayShadowSample(loadSampler(), nowMs)
        saveSampler(decision.next)
        return decision.sample
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
        // `armed`, not `sampled`: asking whether this pass is a sampled one is
        // what *spends* a sample, and a pass that reached here without a row
        // to compare must not spend one on a comparison of nothing.
        if (capture == null || !capture.armed()) return
        // Everything from here is inside one guard, the store call included.
        // `finishPass` is called from a `finally`, so a throw here would
        // replace whatever exception was unwinding the pass -- a family rate
        // limit would surface as a plain failure, the health pill would say
        // the wrong thing, and the retry window would go unlogged. A canary
        // must never be the reason a pass reports a failure.
        try {
            val report = coreRelayShadowCompare(
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
            sink.note(report, nowMs)
            if (report.mismatches.isNotEmpty()) {
                Log.w(
                    TAG,
                    "Relay shadow found ${report.mismatches.size} kind(s) of divergence over " +
                        "${report.stepsCompared} row(s); see the protocol event ring",
                )
            }
        } catch (e: Exception) {
            Log.w(TAG, "Relay shadow comparison failed: ${e.message}")
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
 *
 * @param armSample consulted once, at the first row worth comparing, and
 *   answers whether this pass is a sampled one. A function rather than a flag
 *   so the day's budget is spent on a pass that has evidence in it.
 */
internal class RelayShadowPassCapture(private val armSample: () -> Boolean) {

    private val steps = mutableListOf<CoreRelayShadowStep>()
    private val skipped = mutableListOf<ByteArray>()
    private var unshadowed = 0
    private var dropped = 0
    private var sampled: Boolean? = null

    /**
     * The failed row each mailbox is still waiting to learn the answer for:
     * did this pass go on to offer that mailbox the next row of the same lane?
     * Keyed by lane and endpoint, because two mailboxes' rows interleave in
     * one lane and the question is per mailbox.
     */
    private val awaitingContinuation = mutableMapOf<String, Int>()

    /** A row the legacy engine posted and the relay accepted. */
    fun noteSucceeded(
        lane: CoreRelayShadowLane,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        recipientUserId: ByteArray,
        sealedLen: Int,
        expiryMs: Long,
        endpoint: RelayConfig,
    ) = record(
        lane, msgId, hopTtl, recipientHint, recipientUserId, sealedLen, expiryMs,
        endpoint = endpoint,
        status = 200,
        relayCode = null,
        transportError = null,
        markedPosted = true,
    )

    /** A row the legacy engine posted and the relay or the link refused. */
    fun noteFailed(
        lane: CoreRelayShadowLane,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        recipientUserId: ByteArray,
        sealedLen: Int,
        expiryMs: Long,
        endpoint: RelayConfig,
        error: Exception,
    ) {
        // A family rate limit is wrapped as it unwinds the upload loops, so
        // the relay's own answer is one cause down.
        val http = error as? RelayHttpException ?: error.cause as? RelayHttpException
        record(
            lane, msgId, hopTtl, recipientHint, recipientUserId, sealedLen, expiryMs,
            endpoint = endpoint,
            status = http?.code ?: 0,
            relayCode = http?.relayCode,
            transportError = if (http != null) {
                null
            } else {
                relayClassifyTransportError(error)
            },
            markedPosted = false,
        )
    }

    /** A row the legacy engine declined to post at all, having resolved no mailbox for it. */
    fun noteDeclined(
        lane: CoreRelayShadowLane,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        recipientUserId: ByteArray,
        sealedLen: Int,
        expiryMs: Long,
    ) = record(
        lane, msgId, hopTtl, recipientHint, recipientUserId, sealedLen, expiryMs,
        endpoint = null,
        status = 0,
        relayCode = null,
        transportError = null,
        markedPosted = false,
    )

    @Suppress("LongParameterList")
    private fun record(
        lane: CoreRelayShadowLane,
        msgId: ByteArray,
        hopTtl: UByte,
        recipientHint: ByteArray,
        recipientUserId: ByteArray,
        sealedLen: Int,
        expiryMs: Long,
        endpoint: RelayConfig?,
        status: Int,
        relayCode: String?,
        transportError: CoreRelayTransportError?,
        markedPosted: Boolean,
    ) {
        if (!sampled()) return
        if (steps.size >= MAX_ROWS) {
            // Counted rather than silently forgotten: a report that compared
            // sixteen of forty rows must not read as a report about forty.
            dropped++
            unshadowed++
            return
        }
        val succeeded = markedPosted && status in 200..299
        val key = endpoint?.let { "$lane|${it.relayUrl}|${it.relayToken}" }
        if (key != null) {
            // This row is being offered to a mailbox some earlier row of this
            // lane failed against, which is the whole of what "the lane
            // continued" means. Observed rather than predicted: deriving it
            // from the exception type says "true" even when there was no next
            // row for that mailbox to offer, which is most failures.
            awaitingContinuation.remove(key)?.let { waiting ->
                steps[waiting] = steps[waiting].copy(legacyContinuedLane = true)
            }
        }
        steps += CoreRelayShadowStep(
            lane = lane,
            msgId = msgId,
            hopTtl = hopTtl,
            recipientHint = recipientHint,
            recipientUserId = recipientUserId,
            sealedLen = sealedLen.coerceAtLeast(0).toULong(),
            expiryMs = expiryMs,
            legacyEndpoint = endpoint?.let { CoreRelayEndpointConfig(it.relayUrl, it.relayToken) },
            status = status.coerceIn(0, UShort.MAX_VALUE.toInt()).toUShort(),
            relayCode = relayCode,
            transportError = transportError,
            legacyMarkedPosted = markedPosted,
            // Starts false and is corrected above if a later row is actually
            // offered to the same mailbox. A pass that ends here answered "no".
            legacyContinuedLane = succeeded,
        )
        if (!succeeded && key != null) awaitingContinuation[key] = steps.size - 1
    }

    /**
     * Recipients the legacy engine excluded from its queue query before
     * selecting anything.
     *
     * Buffered whether or not this pass turns out to be sampled, and
     * deliberately not a reason to spend a sample: a device with one retired
     * friend card reports the same skip on every pass it ever runs, so arming
     * on a skip list would arm on every pass and put the budget back where it
     * started.
     */
    fun noteSkippedRecipients(recipients: List<ByteArray>) {
        for (recipient in recipients) {
            if (skipped.size >= MAX_SKIPS) return
            skipped += recipient
        }
    }

    /**
     * Rows this capture deliberately cannot speak for: group fan-out rows,
     * which core's upload lanes do not decompose, and carried rows, which a
     * later package owns.
     *
     * Counted whether or not the sample has been armed yet, and never a reason
     * to arm it. Both halves matter: a mule pass that carries forty rows and
     * compares none is not evidence worth a sample, and a group row that went
     * out before the first authored row would otherwise be missing from a
     * report the authored row does earn -- which is the undercount this field
     * exists to prevent.
     */
    fun noteUnshadowed(rows: Int) {
        if (rows > 0) unshadowed += rows
    }

    /**
     * Whether this pass is one of the sampled ones, deciding it on first ask.
     *
     * Not synchronized: a capture belongs to the one pass that created it and
     * a pass runs on one thread. The sampler state behind [armSample] is the
     * shared thing, and that is guarded where it lives.
     */
    internal fun sampled(): Boolean = sampled ?: armSample().also { sampled = it }

    /**
     * Whether a sample was already spent on this pass. Asks nothing and
     * decides nothing, which is what makes it safe to call at the end of a
     * pass that may have had no rows at all.
     */
    internal fun armed(): Boolean = sampled == true

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

        /** Mirrors `RELAY_SHADOW_MAX_SKIPS`, for the same reason. */
        val MAX_SKIPS = uniffi.cruisemesh_core.coreRelayShadowMaxSkips().toInt()
    }
}
