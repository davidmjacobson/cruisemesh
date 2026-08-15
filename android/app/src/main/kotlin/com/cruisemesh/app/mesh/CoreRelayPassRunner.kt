package com.cruisemesh.app.mesh

import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreRelayAction
import uniffi.cruisemesh_core.CoreRelayActionKind
import uniffi.cruisemesh_core.CoreRelayContactConfig
import uniffi.cruisemesh_core.CoreRelayHttpRequest
import uniffi.cruisemesh_core.CoreRelayHttpResult
import uniffi.cruisemesh_core.CoreRelayPass
import uniffi.cruisemesh_core.CoreRelayPassPlan
import uniffi.cruisemesh_core.CoreRelayPassSummary
import uniffi.cruisemesh_core.CoreRelayProjection
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.recentPresenceHintsFor

private const val TAG = "MeshService"

/**
 * Where one typed relay action is actually performed.
 *
 * A one-method seam, and the whole reason this runner needs no Android at all:
 * the service hands it
 * [com.cruisemesh.app.relay.CoreRelayDriver] pinned to the pass's bound
 * network, and a test hands it a scripted relay. Neither the runner nor the
 * core session can tell the difference, which is what makes a full core pass
 * something a JVM unit test can drive end to end.
 */
internal fun interface RelayActionExecutor {
    fun execute(
        passId: String,
        actionId: ULong,
        request: CoreRelayHttpRequest,
        nowMs: Long,
    ): CoreRelayHttpResult
}

/**
 * Drives a `CoreRelayPass` from its first action to its summary.
 *
 * This is the entire shell-side orchestration of a core relay pass, and its
 * shortness is the point: hand core the plan, ask for an action, do exactly
 * that action, hand back exactly what happened, repeat. There is no branch
 * here on a status code, no retry, no cursor, no marker, no health decision --
 * every one of those has already been made by the time an action arrives, and
 * making any of them here would be the second implementation this program
 * exists to delete.
 *
 * The one judgement it does make is when to stop asking, and it makes it the
 * defensive way. `LIVE-01` says a pass terminates inside its declared budgets,
 * and the core session enforces that; this loop is the backstop for a session
 * that somehow does not, because a driver that spins forever is the failure
 * mode a person experiences as a dead battery rather than as a bug report.
 */
internal class CoreRelayPassRunner(
    private val store: MessageStore,
    private val executor: RelayActionExecutor,
    private val clock: () -> Long,
    private val isCancelled: () -> Boolean = { false },
    /**
     * Where what the pass committed is handed to the surfaces core cannot
     * reach: notifications, the chat list, a contact's "last seen".
     *
     * Called after every result and once more at the end, so a page is
     * projected while it is fresh -- the legacy engine's timing -- rather
     * than in one batch a whole deep-mailbox walk later. It is still not a
     * decision seam: core has already committed every disposition, ack,
     * cursor and marker by the time anything arrives here, and nothing this
     * returns can change the pass.
     */
    private val onProjection: (CoreRelayProjection) -> Unit = {},
) {

    /**
     * Run one pass and return what it did.
     *
     * @param passId a short opaque label a transcript can be read by. Core
     *   derives the id it actually carries from this, so two passes can never
     *   share one however this is called.
     */
    fun run(plan: CoreRelayPassPlan, passId: String): CoreRelayPassSummary {
        val pass = CoreRelayPass(store, plan, passId)
        // Every request the budget permits, plus the acks pages earn, plus
        // room: high enough that no lawful pass reaches it, low enough that an
        // unlawful one is stopped in seconds rather than in a battery.
        val guard = plan.budgets.maxRequests.toLong() * 2 + 64
        var issued = 0L
        var action: CoreRelayAction = pass.start(clock())

        // Every exit drains what the pass committed but has not yet handed
        // over. A cancelled or rate-limited pass has usually still ingested a
        // page, and a message the person never hears about because the pass
        // ended badly is the exact failure this projection exists to prevent.
        fun ended(summary: CoreRelayPassSummary): CoreRelayPassSummary {
            project(pass)
            return summary
        }

        while (true) {
            when (val kind = action.kind) {
                is CoreRelayActionKind.Finished -> return ended(kind.summary)

                // A sleep means the pass refused to spend inside a quiet
                // window and has already finished; the wait itself belongs to
                // whatever schedules the next pass, not to this loop.
                is CoreRelayActionKind.Sleep -> return ended(pass.summary() ?: pass.cancel(clock()))

                // Unreachable after start(), and treated as an ended pass
                // rather than as a reason to call start() again: a second
                // start would re-run stage one against a store the first call
                // already pruned.
                is CoreRelayActionKind.NotStarted -> return ended(pass.cancel(clock()))

                is CoreRelayActionKind.Http -> {
                    if (isCancelled()) return ended(pass.cancel(clock()))
                    if (issued >= guard) {
                        Log.e(
                            TAG,
                            "Core relay pass issued $issued actions without finishing; cancelling",
                        )
                        store.noteInvariantViolation("LIVE-01", "pass_exceeded_driver_guard", clock())
                        return ended(pass.cancel(clock()))
                    }
                    issued++
                    val result = executor.execute(
                        action.passId,
                        action.actionId,
                        kind.request,
                        clock(),
                    )
                    action = pass.resumeHttp(result)
                    project(pass)
                }
            }
        }
    }

    /**
     * Hands over whatever the pass has committed since the last drain.
     *
     * Never allowed to fail the pass. A notification that could not be raised
     * or a "last seen" that could not be moved is a display problem; letting
     * it unwind this loop would abandon a pass whose store writes have all
     * already landed, and cost the mail behind it.
     */
    private fun project(pass: CoreRelayPass) {
        val projection = try {
            pass.takeProjection()
        } catch (e: Exception) {
            Log.w(TAG, "Core relay pass projection could not be read: ${e.message}")
            return
        }
        if (projection.ingested.isEmpty() && projection.presence.isEmpty()) return
        try {
            onProjection(projection)
        } catch (e: Exception) {
            Log.w(TAG, "Core relay pass projection failed: ${e.message}")
        }
    }
}

/**
 * This pass's address book, with both of a contact endpoint's brakes on it.
 *
 * They are two different pieces of evidence about two different failures, and
 * core answers them differently, so they travel separately.
 *
 * [endpointUsable] is *rejection* evidence: the endpoint answered and refused
 * us -- a revoked token, a mailbox that is not ours. An upload for such a
 * contact falls back to this device's own mailbox, because something
 * demonstrably answers there.
 *
 * [endpointAnswering] is *silence* evidence: nothing answered at all -- a host
 * that was retired, an address that no longer resolves. An upload for one of
 * those is declined outright for this pass instead. A quiet host is not proof
 * that somebody else's mailbox is the right place to leave their mail, and the
 * marker such a misroute writes is terminal, so the mistake would be permanent
 * rather than a retry.
 *
 * A function rather than an expression inside the pass builder so the split
 * itself can be tested: folding the two back into one flag is a one-character
 * change with an invisible, permanent consequence, and it is exactly what this
 * call site used to do.
 */
internal fun coreRelayContactConfigs(
    contacts: List<Contact>,
    endpointUsable: (Contact) -> Boolean,
    endpointAnswering: (Contact) -> Boolean,
): List<CoreRelayContactConfig> = contacts.map { contact ->
    CoreRelayContactConfig(
        userId = contact.userId,
        relayUrl = contact.relayUrl,
        relayToken = contact.relayToken,
        endpointUsable = endpointUsable(contact),
        endpointAnswering = endpointAnswering(contact),
    )
}

/**
 * What a core relay pass committed, shown to the person.
 *
 * The core pass ends with everything durable already decided: which rows were
 * persisted, which were acked, where each frontier sits, which endpoint is
 * resting. What it cannot do is open a sealed body -- that needs this
 * device's identity key -- or touch a notification, a chat list or a "last
 * seen". So the split is exactly that: policy stayed in core's transactions,
 * and this projects their result onto Android's surfaces, one page at a time,
 * in the same order and at the same moment the legacy engine does it inline.
 *
 * Deliberately the *same* per-envelope call the legacy walk makes. The
 * disposition it returns is dropped on the floor here, and that is the whole
 * point of the seam: core already decided whether that envelope's relay copy
 * may be acked and already committed the answer, so a second opinion computed
 * here could only disagree with a decision that has shipped. What is wanted
 * from the call is its effects -- open, deliver, notify, refresh, carry-mark
 * -- and having exactly one implementation of those is what makes a device on
 * the core engine indistinguishable from one on the legacy engine.
 */
internal class CoreRelayPassProjector(
    /**
     * The inbound path, as the legacy walk uses it. Its return value is
     * ignored; see the class doc.
     */
    private val deliver: (RelayFetchedEnvelope, Identity) -> Unit,
    /** `MeshConnectivityStatus::mergePresenceLastSeen` in the service. */
    private val mergePresence: (String, Long) -> Unit,
    /** The durable connection-history note the legacy presence sync writes. */
    private val notePresenceSeen: (ByteArray, Long) -> Unit = { _, _ -> },
) {

    /**
     * Projects one drained [CoreRelayProjection].
     *
     * [contacts] is this pass's address book, used only to resolve a mailbox
     * answer -- which names a hint, never a person -- back to whoever that
     * hint belongs to, exactly as the legacy presence sync resolves its own
     * query. A cross-family probe already names the contact and needs no
     * lookup.
     */
    fun project(
        projection: CoreRelayProjection,
        identity: Identity,
        contacts: List<Contact>,
        nowMs: Long,
    ) {
        for (envelope in projection.ingested) {
            try {
                deliver(
                    RelayFetchedEnvelope(
                        id = envelope.id,
                        msgId = envelope.msgId,
                        hopTtl = envelope.hopTtl,
                        recipientHint = envelope.recipientHint,
                        sealed = envelope.sealed,
                        expiryMs = envelope.expiryMs,
                    ),
                    identity,
                )
            } catch (e: Exception) {
                // One envelope that could not be projected must not cost the
                // rest of the page theirs. Nothing durable is lost: core
                // persisted the row, and the frontier it moved was earned by
                // that persistence rather than by anything here.
                Log.w(TAG, "Failed to project a relay-ingested envelope: ${e.message}")
            }
        }
        if (projection.presence.isEmpty()) return
        val contactByHint = HashMap<String, ByteArray>()
        for (contact in contacts) {
            for (hint in recentPresenceHintsFor(contact.userId, nowMs)) {
                contactByHint[UserIdHex.encode(hint)] = contact.userId
            }
        }
        for (observation in projection.presence) {
            val userId = observation.userId.takeIf { it.isNotEmpty() }
                ?: contactByHint[UserIdHex.encode(observation.hint)]
                ?: continue
            // The relay's clock never becomes a local timestamp: core reports
            // how old the answer was, and the age is subtracted from the
            // moment this device observed it -- the same arithmetic the
            // legacy presence sync does.
            val seenAtMs = observation.observedAtMs - observation.ageMs.coerceAtLeast(0L)
            mergePresence(UserIdHex.encode(userId), seenAtMs)
            try {
                notePresenceSeen(userId, seenAtMs)
            } catch (e: Exception) {
                Log.w(TAG, "Failed to record a relay presence sighting: ${e.message}")
            }
        }
    }
}
