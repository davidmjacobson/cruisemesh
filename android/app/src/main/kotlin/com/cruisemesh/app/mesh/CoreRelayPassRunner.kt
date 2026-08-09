package com.cruisemesh.app.mesh

import android.util.Log
import uniffi.cruisemesh_core.CoreRelayAction
import uniffi.cruisemesh_core.CoreRelayActionKind
import uniffi.cruisemesh_core.CoreRelayHttpRequest
import uniffi.cruisemesh_core.CoreRelayHttpResult
import uniffi.cruisemesh_core.CoreRelayPass
import uniffi.cruisemesh_core.CoreRelayPassPlan
import uniffi.cruisemesh_core.CoreRelayPassSummary
import uniffi.cruisemesh_core.MessageStore

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

        while (true) {
            when (val kind = action.kind) {
                is CoreRelayActionKind.Finished -> return kind.summary

                // A sleep means the pass refused to spend inside a quiet
                // window and has already finished; the wait itself belongs to
                // whatever schedules the next pass, not to this loop.
                is CoreRelayActionKind.Sleep -> return pass.summary() ?: pass.cancel(clock())

                // Unreachable after start(), and treated as an ended pass
                // rather than as a reason to call start() again: a second
                // start would re-run stage one against a store the first call
                // already pruned.
                is CoreRelayActionKind.NotStarted -> return pass.cancel(clock())

                is CoreRelayActionKind.Http -> {
                    if (isCancelled()) return pass.cancel(clock())
                    if (issued >= guard) {
                        Log.e(
                            TAG,
                            "Core relay pass issued $issued actions without finishing; cancelling",
                        )
                        store.noteInvariantViolation("LIVE-01", "pass_exceeded_driver_guard", clock())
                        return pass.cancel(clock())
                    }
                    issued++
                    val result = executor.execute(
                        action.passId,
                        action.actionId,
                        kind.request,
                        clock(),
                    )
                    action = pass.resumeHttp(result)
                }
            }
        }
    }
}
