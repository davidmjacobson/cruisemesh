package com.cruisemesh.app.relay

import android.util.Log
import com.cruisemesh.app.mesh.RelaySyncEvents
import uniffi.cruisemesh_core.CoreRelayRotation
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.RelayRotationNextStep
import uniffi.cruisemesh_core.RelayRotationPlan
import uniffi.cruisemesh_core.RevocationCommit
import uniffi.cruisemesh_core.coreMintRelayMemberToken
import uniffi.cruisemesh_core.coreRelayRotationNextStep
import uniffi.cruisemesh_core.corePlanRelayRotation
import uniffi.cruisemesh_core.relayDecodeRotateResponse
import uniffi.cruisemesh_core.relayDepositTokenFor
import uniffi.cruisemesh_core.relayEncodeRotateRequest
import uniffi.cruisemesh_core.relayRetryAfterMs

private const val TAG = "RelayRotation"

/**
 * A call that reached the relay and produced nothing this device can use: a
 * request core would not sign, or an answer that does not describe the
 * rotation that was asked for (a proxy replaying another family's response,
 * a server whose deposit token does not attenuate from the member token it
 * returned). Paced like any other answered failure, because the relay's
 * rotation bucket was charged for it either way, and never committed.
 */
private const val UNUSABLE_ANSWER_STATUS: UShort = 502u

/** Where this device keeps the family relay credential a rotation replaces. */
internal interface RelayRotationCredential {
    fun current(): RelayConfig?

    /** T23 epoch of the current endpoint; a rotation's epoch climbs above it. */
    fun epoch(): Long

    /**
     * Write the rotated endpoint down as this device's own configuration —
     * exactly what scanning a setup card does, which is why it goes through
     * the same store and picks up the same epoch bump.
     */
    fun adopt(config: RelayConfig)
}

/** [RelayRotationCredential] over this install's saved Shore Pass. */
internal class SavedRelayCredential(
    private val context: android.content.Context,
) : RelayRotationCredential {
    override fun current(): RelayConfig? = RelayConfigStore.load(context)

    override fun epoch(): Long = RelayConfigStore.relayEpoch(context)

    override fun adopt(config: RelayConfig) {
        // Durable: this is the only credential that still opens the family
        // mailbox, and the old one is already dead on the server. Losing the
        // write in flight would lock the family out of its own mail.
        RelayConfigStore.save(context, config.relayUrl, config.relayToken, durable = true)
    }
}

/** What one turn of the driver did. Facts, for the log and for tests. */
internal sealed interface RelayRotationOutcome {
    /** No rotation is owed. */
    object NothingPending : RelayRotationOutcome

    /** One is owed, but not yet — [notBeforeMs] is when it may be tried. */
    data class Waiting(val notBeforeMs: Long) : RelayRotationOutcome

    /**
     * The family's relay credential is now the replacement, here and on the
     * server. [alreadyDone] is a retry that found the work already committed —
     * a success, not a failure.
     */
    data class Rotated(val envelopesMoved: ULong, val alreadyDone: Boolean) : RelayRotationOutcome

    /** Not this time; the journal row survives and a later pass tries again. */
    data class Deferred(val step: RelayRotationNextStep) : RelayRotationOutcome

    /** No retry could ever make this rotation happen; the journal was cleared. */
    data class GaveUp(val step: RelayRotationNextStep) : RelayRotationOutcome
}

/**
 * **§10 step 2's driver**: the shell half of rotating the family's shared relay
 * `family_token` after a device is removed.
 *
 * Core owns everything that can be got wrong quietly — minting the replacement
 * ([coreMintRelayMemberToken]), signing the request under the *person root*
 * ([relayEncodeRotateRequest]), refusing an answer about a different family
 * ([relayDecodeRotateResponse]), the crash-safety journal, and what a failed
 * call means ([coreRelayRotationNextStep]). What is left here is the part core
 * cannot do: making the HTTP call, and choosing when.
 *
 * ## The ceremony, and why it is split across two moments
 *
 * [begin] runs on the removal itself, straight after §10.1 commits: it plans
 * the rotation and writes the journal row, and does not touch the network. A
 * removal that could fail on connectivity would be a removal a person has to
 * retry by guessing, and "my phone was stolen" is not a moment to hand somebody
 * a network error. [rotateIfPending] runs from the relay sync pass, which is
 * the only place that already knows how to talk to the family relay — the
 * validated-network rule, the bind target that keeps this off a VPN, the pacing
 * against a family's request budget. So the removal is instant and the rotation
 * lands on the first pass that can reach the relay, which on a connected phone
 * is the one the removal nudges into running a second later.
 *
 * ## Two credentials, one journal row
 *
 * A rotation whose answer was lost leaves the server holding the replacement
 * and this device holding the question. That is why the first ask presents the
 * retired credential and a `401` is not treated as a failure: the only two
 * things that produce it are "the rotation landed and I did not hear" and "this
 * family is gone", and asking again under the replacement tells them apart.
 * relayd answers a repeat presentation with `rotated: false` and the same
 * values, so the second ask converges rather than rotating twice.
 *
 * ## What it will not do
 *
 * It never commits a rotation the relay did not confirm, it never lets the
 * *possession* of a token authorize anything (the signature is over both
 * credentials, under the person root, and relayd pins that key on first use),
 * and it never asks faster than [RelayRotationPacer] allows. That last one is
 * not fussiness: a rerun loop that ignored `Retry-After` once cost a family
 * ~290 posts a minute of its own allowance, and this route is the one a family
 * needs working on the day a phone is stolen.
 *
 * Mirrors iOS `RelayRotationDriver.swift`.
 */
internal class RelayRotationDriver(
    private val store: MessageStore,
    private val credential: RelayRotationCredential,
    /** The HTTP call: bearer + signed body in, response body out. */
    private val rotate: (RelayConfig, ByteArray) -> ByteArray,
    /**
     * Nudge a relay pass, because a rotation leaves work for one: the T23
     * epoch bump this device just made has to reach every contact.
     *
     * Deliberately a nudge rather than the fan-out itself. Adopting the new
     * endpoint is exactly what scanning a setup card does, and the machinery
     * that notices *that* already clears the carried-upload markers, clears
     * the group fan-out markers, queues the `CAP_RELAY_UPDATE` notices and
     * re-scopes the friend directory — four things, in one place, driven off
     * one epoch comparison. A second announcer here would race that
     * comparison and silently take the marker clearing away from it.
     */
    private val onRotated: () -> Unit,
    /**
     * Remember, for the surface to read, whether §10.2 is stuck: true when a
     * relay has refused this device the rotation for good, false whenever a
     * rotation is planned afresh or lands. See [RelayRotationNoticeStore].
     */
    private val notice: (Boolean) -> Unit = {},
    private val pacer: RelayRotationPacer = sharedPacer,
    private val clock: () -> Long = System::currentTimeMillis,
) {

    /**
     * Plan the rotation a revocation just earned and write it down. No network.
     *
     * Returns whether a rotation is now owed — false means this person has no
     * Shore Pass to rotate, which is most installs.
     */
    fun begin(revocation: RevocationCommit): Boolean {
        return beginPlanned { config, now ->
            corePlanRelayRotation(
                revocation,
                config.relayUrl,
                config.relayToken,
                credential.epoch(),
                now,
            )
        }
    }

    /**
     * Opt-in migration for a Shore Pass credential that may have appeared in
     * an older friend card. It deliberately uses the same durable journal as
     * device removal, but does not revoke a device or rotate the inbox key.
     */
    fun beginCredentialRefresh(): Boolean = beginPlanned { config, now ->
        store.planRelayCredentialRefresh(
            config.relayUrl,
            config.relayToken,
            credential.epoch(),
            now,
        )
    }

    private fun beginPlanned(
        planner: (RelayConfig, Long) -> RelayRotationPlan?,
    ): Boolean {
        val now = clock()
        val alreadyPending = try {
            store.pendingRelayRotation()
        } catch (e: Exception) {
            Log.w(TAG, "could not read the pending relay rotation", e)
            return false
        }
        if (alreadyPending != null && describesSavedPass(alreadyPending)) {
            // Deliberately not replaced. A pending row may name a credential
            // the server has already moved to, and overwriting it would throw
            // away the only record of that token -- locking the family out of
            // its own mailbox to lock one thief out. The rotation in flight
            // retires the same shared token this removal wants retired, so
            // finishing it is finishing this one too.
            Log.i(TAG, "A relay rotation is already pending; letting it finish rather than re-minting")
            return true
        }
        if (alreadyPending != null) {
            // A row about a pass this device no longer has. Keeping it would be
            // the worse of the two mistakes twice over: this removal would
            // report a rotation queued and plan nothing, so the token the
            // removed device actually holds would never be re-keyed. Dropping
            // it costs nothing that is still reachable -- see
            // [describesSavedPass].
            Log.i(TAG, "Dropping a pending relay rotation that names a pass this device no longer holds")
            if (!abandon()) return false
        }
        val config = credential.current() ?: return false
        val plan = try {
            planner(config, now)
        } catch (e: Exception) {
            // A deposit-class credential is the only way here: this device
            // cannot fetch its own mail either, so it is misconfigured and
            // rotating is not the repair.
            Log.w(TAG, "this device's relay credential cannot be rotated", e)
            return false
        } ?: return false
        return try {
            store.beginRelayRotation(plan, now)
            // A fresh ceremony starts at the bottom of the ladder: whatever an
            // earlier rotation's failures earned is not this one's to serve --
            // including an older refusal's notice, which is about a rotation
            // this one supersedes.
            pacer.onSettled()
            notice(false)
            Log.i(TAG, "Relay rotation planned; it lands on the next pass that reaches the relay")
            true
        } catch (e: Exception) {
            Log.w(TAG, "could not write the relay rotation down", e)
            false
        }
    }

    /**
     * **§10.2's own-device leg, receiving side.** Pick up a replacement
     * credential a sibling announced.
     *
     * The announcement rides §8's Settings stream, sealed to the fleet's inbox
     * key, and core has already refused an inadmissible entry on the way in (an
     * impossible epoch, an author this roster has buried). All that is left is
     * to write the winner down.
     *
     * **It cannot fire yet, and that is the honest state of this leg**: no
     * shell has a transport for sync records, so nothing but this device's own
     * rotation ever writes that setting, and on this device the setting and the
     * saved pass already agree. It is here rather than owed because the moment
     * WP4's carrier lands, a sibling that slept through a removal repairs itself
     * on its next relay pass with no further change — and because leaving the
     * receiving half unwritten is how a leg ships half-done twice.
     *
     * The guard is deliberately narrow: same relay host, different token, and a
     * pass configured on this device already. A phone whose person deliberately
     * removed its Shore Pass must not have one reinstalled by a fleet
     * announcement, and a phone on a different relay is not the family's to
     * move.
     *
     * Returns whether the saved pass moved, so the pass that called this knows
     * the credential it is holding is stale.
     */
    fun adoptAnnouncedCredential(): Boolean {
        val announced = try {
            store.relayCredentialSetting()
        } catch (e: Exception) {
            Log.w(TAG, "could not read the announced relay credential", e)
            return false
        } ?: return false
        val saved = credential.current() ?: return false
        if (saved.relayUrl != announced.url || saved.relayToken == announced.token) return false
        Log.i(TAG, "Adopting the family relay credential a sibling announced")
        adopt(RelayConfig(announced.url, announced.token))
        return true
    }

    /** Finish whatever [begin] left owed, if the pacer allows an attempt now. */
    fun rotateIfPending(identity: Identity): RelayRotationOutcome {
        val plan = try {
            store.pendingRelayRotation()
        } catch (e: Exception) {
            Log.w(TAG, "could not read the pending relay rotation", e)
            return RelayRotationOutcome.NothingPending
        } ?: run {
            pacer.onSettled()
            return RelayRotationOutcome.NothingPending
        }
        if (!describesSavedPass(plan)) {
            // The pass moved out from under a rotation that never landed. This
            // must be dropped rather than performed: the call would re-key a
            // family this device has left, and committing it would write that
            // family's credential over the pass the person is actually on --
            // reinstalling, in the cleared-pass case, a Shore Pass they
            // deliberately removed.
            Log.i(TAG, "Abandoning a relay rotation that names a pass this device no longer holds")
            abandon()
            pacer.onSettled()
            return RelayRotationOutcome.NothingPending
        }
        val now = clock()
        if (!pacer.mayAttempt(now)) return RelayRotationOutcome.Waiting(pacer.nextAttemptAtMs)

        // The retired credential first. Its rejection is evidence, not failure.
        var answer = ask(plan, identity, bearer = plan.supersededToken, confirming = false)
        if (answer is Ask.Refused && answer.step is RelayRotationNextStep.Confirm) {
            answer = ask(plan, identity, bearer = plan.newToken, confirming = true)
        }
        return when (answer) {
            is Ask.Answered -> commit(plan, answer.rotation)
            is Ask.Refused -> settle(plan, answer.step)
        }
    }

    private sealed interface Ask {
        data class Answered(val rotation: CoreRelayRotation) : Ask
        data class Refused(val step: RelayRotationNextStep) : Ask
    }

    /**
     * Is this journal row still about the pass this device is on?
     *
     * A rotation is planned at the removal and performed whenever the relay is
     * next reachable, which on a ship is days later — and in between, the pass
     * itself can change: a new setup card scanned ashore, a backup restored, or
     * the pass cleared in Advanced. The row names the family it was planned
     * against and nothing reconciles it, so without this check the driver would
     * keep asking a relay that is no longer this family's, and a call that
     * *succeeded* would write the old family's credential over the current one
     * — including reinstalling a pass its person deliberately removed. That is
     * exactly the harm [adoptAnnouncedCredential]'s guard exists to prevent one
     * function away.
     *
     * Both tokens count as a match. A rotation whose commit failed after the
     * server had already re-keyed leaves this device holding the *replacement*
     * with the row still owed, and that row must still be finishable.
     */
    private fun describesSavedPass(plan: RelayRotationPlan): Boolean {
        val saved = credential.current() ?: return false
        if (saved.relayUrl != plan.relayUrl) return false
        return saved.relayToken == plan.supersededToken || saved.relayToken == plan.newToken
    }

    /** Clear the journal row, reporting whether it is actually gone. */
    private fun abandon(): Boolean = try {
        store.abandonRelayRotation()
        true
    } catch (e: Exception) {
        Log.w(TAG, "could not clear the relay rotation", e)
        false
    }

    private fun ask(
        plan: RelayRotationPlan,
        identity: Identity,
        bearer: String,
        confirming: Boolean,
    ): Ask {
        return try {
            // Core signs, over BOTH credentials, with the person root -- never
            // the device key, and never a bare bearer token. relayd registers
            // that key on a family's first rotation and pins it after.
            val body = relayEncodeRotateRequest(bearer, plan.newToken, identity.signSk)
            val response = rotate(RelayConfig(plan.relayUrl, bearer), body)
            Ask.Answered(relayDecodeRotateResponse(response, plan.newToken))
        } catch (e: RelayHttpException) {
            Ask.Refused(
                coreRelayRotationNextStep(
                    e.code.toUShort(),
                    e.relayCode,
                    relayRetryAfterMs(e.retryAfter).toLong(),
                    confirming,
                    pacer.consecutiveFailures.toUInt(),
                ),
            )
        } catch (e: java.io.IOException) {
            // No answer at all, so nothing was charged to the family's rotation
            // bucket and this may be retried in seconds rather than minutes.
            Log.i(TAG, "Relay rotation call did not reach the relay: ${e.message}")
            Ask.Refused(
                coreRelayRotationNextStep(0u, null, 0L, confirming, pacer.consecutiveFailures.toUInt()),
            )
        } catch (e: Exception) {
            Log.w(TAG, "the relay's rotation answer was unusable", e)
            Ask.Refused(
                coreRelayRotationNextStep(
                    UNUSABLE_ANSWER_STATUS,
                    null,
                    0L,
                    confirming,
                    pacer.consecutiveFailures.toUInt(),
                ),
            )
        }
    }

    private fun commit(plan: RelayRotationPlan, rotation: CoreRelayRotation): RelayRotationOutcome {
        val now = clock()
        val committed = try {
            store.commitRelayRotation(plan, now)
        } catch (e: Exception) {
            // The server has already re-keyed the family; only the sibling
            // announcement failed. Adopt anyway -- this device must not be
            // locked out of a mailbox it can still open -- and leave the
            // journal row for a later pass to finish publishing.
            Log.e(TAG, "the rotated credential could not be announced to this person's other devices", e)
            adopt(RelayConfig(plan.relayUrl, plan.newToken))
            pacer.onFailure(now, retryDelayMs())
            return RelayRotationOutcome.Deferred(RelayRotationNextStep.Retry(retryDelayMs()))
        }
        adopt(RelayConfig(committed.endpoint.url, committed.endpoint.token))
        pacer.onSettled()
        notice(false)
        Log.i(
            TAG,
            "Family relay credential rotated (rotated=${rotation.rotated}, " +
                "${rotation.envelopesMoved} envelope(s) carried across, " +
                "${committed.contactUserIds.size} contact(s) to tell)",
        )
        return RelayRotationOutcome.Rotated(rotation.envelopesMoved, alreadyDone = !rotation.rotated)
    }

    /**
     * Write the credential down, and ask for a pass to carry the consequences.
     *
     * Contacts are told by the shipped T23 path, unchanged: saving bumps this
     * device's relay epoch, and the next pass fans the *deposit* attenuation
     * of the new token out to every one of them. Core's
     * `encodeRelayUpdateContent` attenuates unconditionally, so no leg of this
     * can put a member token on a contact's phone.
     */
    private fun adopt(config: RelayConfig) {
        credential.adopt(config)
        onRotated()
    }

    private fun settle(plan: RelayRotationPlan, step: RelayRotationNextStep): RelayRotationOutcome {
        val now = clock()
        return when (step) {
            is RelayRotationNextStep.Retry -> {
                pacer.onFailure(now, step.delayMs)
                RelayRotationOutcome.Deferred(step)
            }
            is RelayRotationNextStep.Remint -> {
                // Astronomically unlikely (32 bytes of OS randomness collided
                // with a credential this relay already holds), but the answer
                // is cheap and retrying the same token converges on nothing.
                val minted = coreMintRelayMemberToken()
                try {
                    store.beginRelayRotation(
                        plan.copy(newToken = minted, newDepositToken = relayDepositTokenFor(minted)),
                        now,
                    )
                } catch (e: Exception) {
                    Log.w(TAG, "could not re-mint the replacement credential", e)
                }
                pacer.onFailure(now, retryDelayMs())
                RelayRotationOutcome.Deferred(step)
            }
            is RelayRotationNextStep.Confirm -> {
                // Only reachable if a confirming ask somehow answered "confirm"
                // again; core does not, but treating it as a wait keeps this
                // from becoming a loop if that ever changes.
                pacer.onFailure(now, retryDelayMs())
                RelayRotationOutcome.Deferred(step)
            }
            is RelayRotationNextStep.ServerManagedToken,
            is RelayRotationNextStep.NotTheAuthority,
            -> {
                // Neither can ever succeed from this device, so the honest
                // thing is to stop asking. The device keeps the credential it
                // has; the removed phone keeps it too, and the repair is a new
                // token from whoever can issue one.
                Log.e(
                    TAG,
                    "This relay will not let this device rotate the family token ($step); " +
                        "the removed device keeps its relay credential until the pass is replaced",
                )
                abandon()
                // And say so. The removal confirmation promised this person
                // that the removed phone loses the family mailbox; a promise
                // that quietly could not be kept is worse than one that was
                // never made, and only the person can act on it -- the repair
                // is a new pass from whoever can issue one.
                notice(true)
                pacer.onSettled()
                RelayRotationOutcome.GaveUp(step)
            }
        }
    }

    /** Core's ladder for an answered failure, asked for without an answer to read. */
    private fun retryDelayMs(): Long =
        when (
            val step = coreRelayRotationNextStep(
                UNUSABLE_ANSWER_STATUS,
                null,
                0L,
                true,
                pacer.consecutiveFailures.toUInt(),
            )
        ) {
            is RelayRotationNextStep.Retry -> step.delayMs
            else -> 0L
        }

    companion object {
        /**
         * Process-wide, because the thing being paced is this device's calls to
         * one relay and the driver is built fresh on every pass.
         */
        val sharedPacer = RelayRotationPacer()

        /**
         * The driver as the app builds it: the saved Shore Pass, the shared
         * relay HTTP client bound to [network] (the pass's validated-network
         * choice — never a VPN), and the ordinary sync nudge.
         */
        fun forApp(
            context: android.content.Context,
            store: MessageStore,
            network: android.net.Network? = null,
        ): RelayRotationDriver = RelayRotationDriver(
            store = store,
            credential = SavedRelayCredential(context.applicationContext),
            rotate = { config, body -> RelayClient.rotateFamilyToken(config, body, network) },
            onRotated = RelaySyncEvents::requestSync,
            notice = { blocked ->
                RelayRotationNoticeStore.setBlocked(context.applicationContext, blocked)
            },
        )
    }
}
