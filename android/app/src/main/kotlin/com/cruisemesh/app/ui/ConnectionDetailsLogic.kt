package com.cruisemesh.app.ui

import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.mesh.MeshRouterState
import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.cruisemesh_core.CoreConnectionHealth
import uniffi.cruisemesh_core.CoreConnectionHealthInput
import uniffi.cruisemesh_core.CoreDeliveryBlockedReason
import uniffi.cruisemesh_core.CoreDeliveryLine
import uniffi.cruisemesh_core.CoreDirectLink
import uniffi.cruisemesh_core.CoreDirectPathState
import uniffi.cruisemesh_core.CoreHealthAction
import uniffi.cruisemesh_core.CoreHealthReason
import uniffi.cruisemesh_core.CoreMeshRuntime
import uniffi.cruisemesh_core.CorePersonAttention
import uniffi.cruisemesh_core.CorePersonHealthInput
import uniffi.cruisemesh_core.CorePersonReach
import uniffi.cruisemesh_core.CorePersonRoute
import uniffi.cruisemesh_core.CoreRecipientDeliveryInput
import uniffi.cruisemesh_core.CoreRelayPathState
import uniffi.cruisemesh_core.PeerConnectionTransport
import uniffi.cruisemesh_core.coreClassifyConnectionHealth
import uniffi.cruisemesh_core.coreClassifyRecipientDelivery
import uniffi.cruisemesh_core.coreConnectionCheckPending
import uniffi.cruisemesh_core.coreContactEndpointResting
import uniffi.cruisemesh_core.coreGroupPeople
import uniffi.cruisemesh_core.corePersonBestRoute

/**
 * Everything the Connection details page decides *before* it touches Compose.
 *
 * The interpretation itself is not here -- it is in the core
 * (`core/src/connection_health.rs`), so Android and iOS cannot drift apart.
 * What lives here is the narrow shell-side work the core deliberately does not
 * do: turning this platform's observable signals into the core's inputs,
 * turning the core's answer plus the store snapshot into a flat view state,
 * and the two pure UI policies the core has no opinion on -- how fresh a
 * timestamp reads, and when a burst of store-change signals is allowed to
 * cause a reload.
 *
 * Nothing in this file imports Android or Compose, so all of it is unit
 * tested directly. Nothing in it produces user-facing text either: the view
 * state carries enums and counts, and `ConnectionDetailsScreen` renders them
 * through `strings.xml`, where the localization gate can see the copy.
 */

// ---------------------------------------------------------------------------
// View state
// ---------------------------------------------------------------------------

/** Which path a badge or path row names. */
enum class ConnectionPathBadge { BLUETOOTH, LOCAL_WIFI, SHORE_PASS }

/**
 * The status sentence under a person's name.
 *
 * `NoHistory` is a first-class case rather than a missing timestamp, because
 * "friend added five minutes ago, never met yet" must read as itself and never
 * as a date derived from a zero.
 */
sealed interface PersonStatus {
    /** A live direct link exists right now. */
    data object ConnectedNow : PersonStatus

    /** No live link, but their relay presence is fresh and our own pass works. */
    data class SeenOnline(val atMs: Long) : PersonStatus

    /** The newest recorded evidence for this person. */
    data class History(val evidence: PeerEvidence, val atMs: Long) : PersonStatus

    /** Nothing has ever been recorded for this person. */
    data object NoHistory : PersonStatus
}

/**
 * The informational expansion under a person row.
 *
 * Everything here is a restatement, never a control: the spec forbids a manual
 * transport picker, and [bestRoute] in particular is the core's routing answer
 * ([corePersonBestRoute]) rather than anything this page worked out. A page
 * that re-derived reachability from "can I poll them" would report post-only
 * friend cards as broken, which is the failure the core answer exists to
 * prevent.
 */
data class PersonDetail(
    val bestRoute: CorePersonRoute,
    /** Freshest evidence their device was alive, epoch ms; `0` when none. */
    val lastSeenMs: Long,
    /** Their delivery receipt for one of our messages, epoch ms; `0` when none. */
    val lastDeliveredMs: Long,
)

data class ConnectionPersonRow(
    val userIdHex: String,
    val name: String,
    val status: PersonStatus,
    val badge: ConnectionPathBadge?,
    /**
     * What is still outstanding for this person, as the core classified it
     * ([coreClassifyRecipientDelivery]); null when there is nothing to say.
     */
    val delivery: CoreDeliveryLine?,
    /**
     * Why they are in Needs attention. The same verdict as [delivery]'s, since
     * both come out of the one classification, so a row cannot sit in Needs
     * attention over a problem its own delivery line does not mention.
     */
    val attention: CorePersonAttention?,
    val detail: PersonDetail,
)

data class ConnectionActivityRow(
    /** Null when the event belongs to an identity that is not a contact. */
    val name: String?,
    val evidence: PeerEvidence,
    val transport: PeerConnectionTransport,
    val atMs: Long,
)

/**
 * The health card's facts. The states, reasons, and actions are the core's --
 * this record only carries them to the renderer.
 */
data class HealthCardState(
    val state: CoreConnectionHealth,
    val nearbyFriendCount: Int,
    val bluetooth: CoreDirectPathState,
    val relay: CoreRelayPathState,
    val reason: CoreHealthReason?,
    val action: CoreHealthAction?,
)

/**
 * *This phone's* paths. A friend's endpoint problem is that friend's row and
 * never a row here -- mixing the two is how the old page manufactured
 * contradictions.
 */
data class PathsCardState(
    val bluetooth: CoreDirectPathState,
    val bluetoothLinks: Int,
    val bluetoothAudioActive: Boolean,
    val localWifiLinks: Int,
    val relay: CoreRelayPathState,
    /** Last successful Shore Pass sync, epoch ms; `0` when there has been none. */
    val relayLastSyncMs: Long,
)

data class ConnectionDetailsState(
    val health: HealthCardState,
    val paths: PathsCardState,
    val needsAttention: List<ConnectionPersonRow>,
    val reachableNow: List<ConnectionPersonRow>,
    val otherPeople: List<ConnectionPersonRow>,
    val hasContacts: Boolean,
    val activity: List<ConnectionActivityRow>,
    /** Epoch ms the snapshot behind this state was loaded; `0` before the first load. */
    val updatedAtMs: Long,
    val refreshing: Boolean,
)

// ---------------------------------------------------------------------------
// Store snapshot (produced off the main thread, consumed here)
// ---------------------------------------------------------------------------

/**
 * What one person's outgoing mail looks like, straight from the core's
 * per-recipient read model (`MessageStore::recipient_delivery_status`).
 *
 * Facts only, and every one of them is passed to the core untouched. In
 * particular the four endpoint-health numbers are *not* interpreted here: the
 * streak thresholds and rest windows belong to `contact_relay_health`, and a
 * shell that reproduced any of them would be the start of the next drift.
 */
data class PersonDeliveryFacts(
    val waitingCount: Int,
    /**
     * How much of [waitingCount] this phone has not managed to hand over yet.
     *
     * Zero, with messages still waiting, means this phone has done everything
     * it can and the other one has not collected -- ordinary store-and-forward,
     * never a stall. The core gates the delayed line on it.
     */
    val unpostedWaitingCount: Int,
    val oldestWaitingMs: Long,
    val lastProgressMs: Long,
    val oversizedWaiting: Boolean,
    val relayRejectStreak: Long,
    val relayRejectedAtMs: Long,
    val relayUnreachableStreak: Long,
    val relayUnreachableAtMs: Long,
) {
    companion object {
        /** Nothing outstanding and no endpoint trouble: the ordinary case. */
        val NONE = PersonDeliveryFacts(0, 0, 0L, 0L, false, 0L, 0L, 0L, 0L)
    }
}

/** One person as the background load found them. */
data class ConnectionPerson(
    val userIdHex: String,
    val userId: ByteArray,
    val name: String,
    val blocked: Boolean,
    /** Their friend card carries an internet-delivery endpoint. */
    val hasRelayEndpoint: Boolean,
    val delivery: PersonDeliveryFacts,
    /** Newest recorded evidence across every path, or null when there is none. */
    val latest: PeerStatusLine?,
    /** Their newest delivery receipt for one of our messages; `0` when none. */
    val lastDeliveredMs: Long,
) {
    // Data classes with an array member need these spelled out; the generated
    // ones compare identity, which would make two equal snapshots look
    // different and cause a pointless recomposition on every reload.
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ConnectionPerson) return false
        return userIdHex == other.userIdHex &&
            name == other.name &&
            blocked == other.blocked &&
            hasRelayEndpoint == other.hasRelayEndpoint &&
            delivery == other.delivery &&
            latest == other.latest &&
            lastDeliveredMs == other.lastDeliveredMs
    }

    override fun hashCode(): Int {
        var result = userIdHex.hashCode()
        result = 31 * result + name.hashCode()
        result = 31 * result + blocked.hashCode()
        result = 31 * result + hasRelayEndpoint.hashCode()
        result = 31 * result + delivery.hashCode()
        result = 31 * result + (latest?.hashCode() ?: 0)
        result = 31 * result + lastDeliveredMs.hashCode()
        return result
    }
}

data class ConnectionStoreSnapshot(
    val people: List<ConnectionPerson>,
    val activity: List<ConnectionActivityRow>,
    val loadedAtMs: Long,
) {
    companion object {
        val EMPTY = ConnectionStoreSnapshot(emptyList(), emptyList(), 0L)
    }
}

// ---------------------------------------------------------------------------
// Platform signals -> core inputs
// ---------------------------------------------------------------------------

/**
 * Translates this platform's observable signals into the core's vocabulary.
 *
 * Every function here is a mapping, never a decision. The moment one of them
 * starts deciding something, the two shells have started to drift again.
 */
object ConnectionInputs {

    fun runtime(state: MeshRuntimeState): CoreMeshRuntime = when (state) {
        MeshRuntimeState.STOPPED -> CoreMeshRuntime.STOPPED
        MeshRuntimeState.STARTING -> CoreMeshRuntime.STARTING
        MeshRuntimeState.ACTIVE -> CoreMeshRuntime.ACTIVE
        MeshRuntimeState.NO_BLUETOOTH -> CoreMeshRuntime.BLUETOOTH_OFF
    }

    /**
     * Bluetooth availability. `NO_BLUETOOTH` is the runtime telling us the
     * radio is off even though the service is up -- exactly the state a person
     * lands in after toggling Bluetooth and forgetting to toggle it back.
     */
    fun bluetooth(state: MeshRuntimeState): CoreDirectPathState = when (state) {
        MeshRuntimeState.STOPPED, MeshRuntimeState.NO_BLUETOOTH -> CoreDirectPathState.OFF
        MeshRuntimeState.STARTING -> CoreDirectPathState.STARTING
        MeshRuntimeState.ACTIVE -> CoreDirectPathState.AVAILABLE
    }

    /**
     * Local Wi-Fi availability, from whether the LAN transport actually holds
     * a listening socket -- not from whether the service is nominally running.
     *
     * Only the *existence* of the endpoint is read. The endpoint itself is
     * never carried into the view state, let alone rendered: addresses and
     * network names stay off this page.
     */
    fun localWifi(state: MeshRuntimeState, listening: Boolean): CoreDirectPathState = when {
        state == MeshRuntimeState.STOPPED -> CoreDirectPathState.OFF
        state == MeshRuntimeState.STARTING -> CoreDirectPathState.STARTING
        listening -> CoreDirectPathState.AVAILABLE
        else -> CoreDirectPathState.OFF
    }

    /**
     * Our own Shore Pass path.
     *
     * [RelayHealth.MessageTooLarge] maps to `Connected` on purpose: an
     * oversized envelope is a fact about one message and one recipient, and
     * the spec keeps it out of the path states entirely. The service is
     * reachable and every other message is still moving; saying the pass is
     * broken there is the old page's mistake.
     */
    fun relay(health: RelayHealth, configured: Boolean): CoreRelayPathState {
        if (!configured) return CoreRelayPathState.NOT_SET_UP
        return when (health) {
            // A saved pass with no published verdict yet. That happens on
            // every cold start before the first check lands and again after
            // the service tears its status down, and reading it as "not set
            // up" tells a person with a working pass to go and buy one --
            // which is what the Shore Pass screen's own flicker machinery
            // exists to avoid saying.
            is RelayHealth.NoConfig -> CoreRelayPathState.CHECKING
            is RelayHealth.Checking -> CoreRelayPathState.CHECKING
            is RelayHealth.NoInternet -> CoreRelayPathState.WAITING_FOR_INTERNET
            is RelayHealth.DeferredRoaming -> CoreRelayPathState.WAITING_FOR_INTERNET
            is RelayHealth.Ok -> CoreRelayPathState.CONNECTED
            is RelayHealth.Failing -> CoreRelayPathState.UNREACHABLE
            is RelayHealth.Expired -> CoreRelayPathState.PASS_EXPIRED
            is RelayHealth.ExpiredReadOnly -> CoreRelayPathState.PASS_EXPIRED_READ_ONLY
            is RelayHealth.Suspended -> CoreRelayPathState.PASS_SUSPENDED
            is RelayHealth.TokenRejected -> CoreRelayPathState.SETUP_REJECTED
            is RelayHealth.QuotaFull -> CoreRelayPathState.STORAGE_FULL
            is RelayHealth.MessageTooLarge -> CoreRelayPathState.CONNECTED
            is RelayHealth.RateLimited -> CoreRelayPathState.SYNCING_SLOWED
        }
    }

    /**
     * [RelayHealth.NoInternet] is the only "no validated internet" verdict the
     * app publishes; every other health value was produced by a request that
     * a validated network carried.
     */
    fun validatedInternet(health: RelayHealth): Boolean =
        health !is RelayHealth.NoInternet && health !is RelayHealth.DeferredRoaming

    /** Last successful Shore Pass sync, or `0` when there has not been one. */
    fun relayLastSyncMs(health: RelayHealth): Long =
        (health as? RelayHealth.Ok)?.lastSyncMs ?: 0L

    fun directLink(transport: MeshRouterState.Transport?): CoreDirectLink? = when (transport) {
        MeshRouterState.Transport.LAN -> CoreDirectLink.LOCAL_WIFI
        MeshRouterState.Transport.CENTRAL, MeshRouterState.Transport.PERIPHERAL ->
            CoreDirectLink.BLUETOOTH
        null -> null
    }
}

/**
 * Holds the moment an unresolved check began, so the core can bound how long
 * the card may say `Checking`.
 *
 * A single mutable long, because a mark that restarts on every recomposition
 * would make the bound unreachable and pin the card in Checking forever --
 * which is the failure the bound exists to prevent.
 */
class CheckingClock {
    private var sinceMs = 0L

    /** @return the epoch ms the current check started, or `0` when nothing is pending. */
    fun mark(pending: Boolean, nowMs: Long): Long {
        if (!pending) {
            sinceMs = 0L
            return 0L
        }
        if (sinceMs == 0L) sinceMs = nowMs
        return sinceMs
    }
}

/**
 * Is some path still coming up, with no verdict on it yet?
 *
 * The answer is the core's ([coreConnectionCheckPending]) because the same
 * question is asked inside the classification, and a shell that asked a
 * narrower one would start the bounded-Checking clock late -- or never -- and
 * show a failure before the check that would prove it had finished.
 */
fun connectionCheckPending(
    runtime: CoreMeshRuntime,
    bluetooth: CoreDirectPathState,
    localWifi: CoreDirectPathState,
    relay: CoreRelayPathState,
): Boolean = coreConnectionCheckPending(runtime, bluetooth, localWifi, relay)

// ---------------------------------------------------------------------------
// Freshness and event times
// ---------------------------------------------------------------------------

/** How the health card's `Updated …` label reads. */
sealed interface FreshnessLabel {
    /** Nothing has loaded yet, so there is nothing honest to date. */
    data object Never : FreshnessLabel
    data object JustNow : FreshnessLabel
    data class Minutes(val value: Int) : FreshnessLabel
    data class Hours(val value: Int) : FreshnessLabel
}

/**
 * How long a message has been waiting, for the `· 14 min` half of a delayed or
 * blocked delivery line.
 *
 * Deliberately not [EventTime]: that renders a *moment* ("14 min ago",
 * "yesterday at 8:03 PM"), and an age is a duration. Reusing it would put "ago"
 * inside a sentence that already reads as elapsed time, and would eventually
 * put a calendar date there — `2 messages delayed · on 3/14/26` says nothing a
 * reader can use.
 */
sealed interface WaitingAge {
    /** Unusable or under a minute. The line renders with no age at all. */
    data object Unknown : WaitingAge
    data class Minutes(val value: Int) : WaitingAge
    data class Hours(val value: Int) : WaitingAge
    data class Days(val value: Int) : WaitingAge
}

/** How a recorded moment reads in a person row or an activity line. */
sealed interface EventTime {
    /** Zero, negative, or otherwise unusable. Renders as no time at all. */
    data object Unknown : EventTime
    data object JustNow : EventTime
    data class Minutes(val value: Int) : EventTime
    data class Hours(val value: Int) : EventTime
    data object Yesterday : EventTime
    data object Older : EventTime
}

object ConnectionTimes {
    const val MINUTE_MS = 60_000L
    const val HOUR_MS = 60 * MINUTE_MS
    const val DAY_MS = 24 * HOUR_MS

    /**
     * The health card's freshness label.
     *
     * A snapshot stamped in the future is a clock artifact, not a reason to
     * render a negative age, so it reads as just now.
     */
    fun freshness(updatedAtMs: Long, nowMs: Long): FreshnessLabel {
        if (updatedAtMs <= 0L) return FreshnessLabel.Never
        val age = nowMs - updatedAtMs
        if (age < MINUTE_MS) return FreshnessLabel.JustNow
        if (age < HOUR_MS) return FreshnessLabel.Minutes((age / MINUTE_MS).toInt())
        return FreshnessLabel.Hours((age / HOUR_MS).toInt())
    }

    /**
     * How one recorded moment reads.
     *
     * The spec asks for relative time inside a day, `Yesterday` when it
     * applies, and a localized short date otherwise. Those two rules can
     * disagree -- 8:03 PM yesterday seen at 10 AM today is under 24 hours old
     * *and* yesterday -- so the calendar day wins, which is the reading the
     * spec's own example ("Last connected yesterday at 8:03 PM") asks for.
     *
     * @param startOfTodayMs local midnight, supplied by the caller because a
     *   calendar needs a time zone and this file stays free of platform types.
     */
    fun eventTime(atMs: Long, nowMs: Long, startOfTodayMs: Long): EventTime {
        if (atMs <= 0L) return EventTime.Unknown
        if (atMs >= nowMs) return EventTime.JustNow
        if (atMs >= startOfTodayMs) {
            val age = nowMs - atMs
            if (age < MINUTE_MS) return EventTime.JustNow
            if (age < HOUR_MS) return EventTime.Minutes((age / MINUTE_MS).toInt())
            return EventTime.Hours((age / HOUR_MS).toInt())
        }
        if (atMs >= startOfTodayMs - DAY_MS) return EventTime.Yesterday
        return EventTime.Older
    }

    /**
     * How long something queued at [sinceMs] has been waiting.
     *
     * An unset stamp and a stamp in the future both come back [WaitingAge
     * .Unknown]: the second is a clock artifact, and the alternative is a
     * negative age rendered as an enormous one. Under a minute is Unknown too,
     * because `2 messages delayed · 0 min` reads as a bug.
     */
    fun waitingAge(sinceMs: Long, nowMs: Long): WaitingAge {
        if (sinceMs <= 0L || nowMs < sinceMs) return WaitingAge.Unknown
        val age = nowMs - sinceMs
        if (age < MINUTE_MS) return WaitingAge.Unknown
        if (age < HOUR_MS) return WaitingAge.Minutes((age / MINUTE_MS).toInt())
        if (age < DAY_MS) return WaitingAge.Hours((age / HOUR_MS).toInt())
        return WaitingAge.Days((age / DAY_MS).toInt())
    }
}

// ---------------------------------------------------------------------------
// Refresh coalescing
// ---------------------------------------------------------------------------

/** The window a burst of store-change signals collapses into one reload. */
const val CONNECTION_COALESCE_WINDOW_MS = 500L

/**
 * The page's reload policy: coalesced, single-flight, and never more than one
 * follow-up owed.
 *
 * This page reads the same store and the same change stream that, undebounced
 * and reloaded on the main thread, has already driven the app into
 * input-dispatch ANRs during a mesh flood. Thousands of signals a minute is a
 * normal condition here, not a stress test, so the policy is a first-class
 * object with its own tests rather than a `delay()` somewhere in a composable.
 *
 * Holds no clock and no threads: the caller passes `nowMs` and does the
 * waiting, which is what makes the whole thing testable.
 */
class StoreChangeCoalescer(private val windowMs: Long = CONNECTION_COALESCE_WINDOW_MS) {
    private var inFlight = false
    private var followUp = false
    private var windowEndsAtMs: Long? = null

    /**
     * A store-change signal arrived.
     *
     * @return true when the caller now owns a reload window and should wait it
     *   out ([remainingMs]) before loading; false when this signal was absorbed
     *   -- either by a window already open, or by a reload already running, in
     *   which case exactly one follow-up is remembered.
     */
    fun onSignal(nowMs: Long): Boolean {
        if (inFlight) {
            followUp = true
            return false
        }
        if (windowEndsAtMs != null) return false
        windowEndsAtMs = nowMs + windowMs
        return true
    }

    /**
     * How long is still owed to the open window; `0` once it has elapsed.
     *
     * Clamped to the window length so a clock that jumps backwards cannot
     * stall the page behind an enormous wait.
     */
    fun remainingMs(nowMs: Long): Long {
        val ends = windowEndsAtMs ?: return 0L
        return (ends - nowMs).coerceIn(0L, windowMs)
    }

    fun onReloadStarted() {
        windowEndsAtMs = null
        inFlight = true
        // Signals that arrived during the wait are already covered by the load
        // that is about to read the store; only ones arriving from here on are
        // owed a follow-up.
        followUp = false
    }

    /** @return true when at least one signal arrived mid-reload and is owed a follow-up. */
    fun onReloadFinished(): Boolean {
        inFlight = false
        val owed = followUp
        followUp = false
        return owed
    }

    /**
     * Forget any window or reload this object still believes is outstanding.
     *
     * Called when the loop that drives it starts, because the loop can be
     * cancelled mid-window or mid-load -- the page is paused every time the
     * screen locks -- and this object outlives it. Without the reset it would
     * spend the rest of the composition absorbing every signal as "a reload is
     * already running", and the page would never load again: frozen rows, a
     * freshness label that keeps ageing, and a pull-to-refresh spinner with
     * nothing behind it.
     */
    fun reset() {
        inFlight = false
        followUp = false
        windowEndsAtMs = null
    }
}

/**
 * The page's reload loop: seed, poll, coalesce, load, repeat.
 *
 * Lives here rather than inside the composable so the seam between the loop
 * and its policy has a test. Everything that made the seam worth extracting is
 * a *lifecycle* property -- what survives a pause, what is owed after a
 * cancellation -- and none of it is observable from [StoreChangeCoalescer]'s
 * own unit tests, which pass with the loop wired up wrongly.
 *
 * Runs on the caller's dispatcher and does no store work itself: [load] is
 * handed in, and its implementation is where the background dispatcher is
 * chosen. That keeps "no store query on the main thread" a property visible at
 * the call site instead of an assumption buried in here.
 *
 * Never returns normally; it ends only by cancellation.
 */
@Suppress("LongParameterList")
internal suspend fun runConnectionRefreshLoop(
    coalescer: StoreChangeCoalescer,
    requests: Channel<Unit>,
    signal: () -> Unit,
    nowMs: () -> Long,
    pollIntervalMs: Long,
    onRefreshingChanged: (Boolean) -> Unit,
    load: suspend () -> ConnectionStoreSnapshot,
    onLoaded: (ConnectionStoreSnapshot) -> Unit,
) {
    // Before anything else: a previous run of this loop may have been
    // cancelled part-way through a window or a load.
    coalescer.reset()
    // Seed with a window that has already elapsed: the first paint should not
    // wait out a debounce nobody asked for.
    if (coalescer.onSignal(nowMs() - CONNECTION_COALESCE_WINDOW_MS)) requests.trySend(Unit)
    coroutineScope {
        // Polling fallback until the store publishes a change signal. The same
        // three rules apply: a tick that lands mid-reload is absorbed, not
        // stacked.
        launch {
            while (true) {
                delay(pollIntervalMs)
                signal()
            }
        }
        while (true) {
            requests.receive()
            var wait = coalescer.remainingMs(nowMs())
            while (wait > 0L) {
                delay(wait)
                wait = coalescer.remainingMs(nowMs())
            }
            coalescer.onReloadStarted()
            onRefreshingChanged(true)
            val loaded = load()
            // The last good snapshot stayed on screen throughout; it is
            // replaced only once a whole new one exists.
            onLoaded(loaded)
            onRefreshingChanged(false)
            if (coalescer.onReloadFinished()) signal()
        }
    }
}

// ---------------------------------------------------------------------------
// Delivery language
// ---------------------------------------------------------------------------

object DeliveryPresentation {
    /**
     * The delivery verdict for one person, or null when there is nothing
     * honest to say.
     *
     * Every part of the decision is the core's ([coreClassifyRecipientDelivery])
     * -- the route-usability predicate, the delayed window, which faults become
     * an error row, and which of those puts a person in Needs attention. All
     * that happens here is handing over the store's facts and this device's
     * path state.
     *
     * The count arriving here is already receipt-aware, which is what makes
     * "Received your message 12 min ago" and a waiting line unable to appear
     * together: not a special case that suppresses the second, but nothing
     * left to count. The Phase 1 front end that had to suppress it by hand
     * (`core_classify_delivery_line`) stays exported, and pinned by its own
     * Rust test, as the documented narrow door onto the one decision
     * procedure; neither shell calls it any more.
     *
     * @param directLink a live direct link to this person exists right now.
     * @param ownRelayUsable our own Shore Pass path can deliver
     *   (`CoreConnectionEvidence.own_relay_usable`).
     * @param relay this phone's normalized Shore Pass path
     *   (`CoreConnectionEvidence.relay`).
     */
    fun line(
        person: ConnectionPerson,
        directLink: Boolean,
        ownRelayUsable: Boolean,
        relay: CoreRelayPathState,
        nowMs: Long,
    ): CoreDeliveryLine? = coreClassifyRecipientDelivery(
        CoreRecipientDeliveryInput(
            waitingCount = person.delivery.waitingCount.coerceAtLeast(0).toUInt(),
            unpostedWaitingCount = person.delivery.unpostedWaitingCount
                .coerceAtLeast(0)
                .toUInt(),
            oldestWaitingMs = person.delivery.oldestWaitingMs,
            lastProgressMs = person.delivery.lastProgressMs,
            oversizedWaiting = person.delivery.oversizedWaiting,
            relayRejectStreak = person.delivery.relayRejectStreak,
            relayRejectedAtMs = person.delivery.relayRejectedAtMs,
            relayUnreachableStreak = person.delivery.relayUnreachableStreak,
            relayUnreachableAtMs = person.delivery.relayUnreachableAtMs,
            relay = relay,
            ownRelayUsable = ownRelayUsable,
            contactHasRelayEndpoint = person.hasRelayEndpoint,
            directLink = directLink,
            nowMs = nowMs,
        ),
    )

    /**
     * How a message to this person would travel right now, asked of the core
     * rather than worked out here.
     *
     * The endpoint-resting half is [coreContactEndpointResting], the same
     * predicate the delivery classification consults, so the person detail's
     * route sentence and the delivery line under their name are two readings
     * of one answer.
     */
    fun bestRoute(
        person: ConnectionPerson,
        directLink: CoreDirectLink?,
        ownRelayUsable: Boolean,
        nowMs: Long,
    ): CorePersonRoute = corePersonBestRoute(
        directLink,
        ownRelayUsable,
        person.hasRelayEndpoint,
        coreContactEndpointResting(
            person.delivery.relayRejectStreak,
            person.delivery.relayRejectedAtMs,
            person.delivery.relayUnreachableStreak,
            person.delivery.relayUnreachableAtMs,
            nowMs,
        ),
    )
}

/**
 * What a `How to fix` control opens onto.
 *
 * Two sources, one destination: the health card's device-wide fault and a
 * person row's per-recipient one. They are separate types in the core because
 * one is about this phone and the other about one friend -- the distinction
 * that stops a friend's broken card turning the whole page red -- and the
 * shell keeps them apart for the same reason rather than flattening them into
 * a single reason enum.
 */
sealed interface HowToFixTopic {
    /** A fault with this device's own connection. */
    data class Device(val reason: CoreHealthReason) : HowToFixTopic

    /** A fault stopping delivery to one friend, named so the copy can say who. */
    data class Person(val reason: CoreDeliveryBlockedReason, val name: String) : HowToFixTopic
}

// ---------------------------------------------------------------------------
// View-state assembly
// ---------------------------------------------------------------------------

/** Contacts read per reload. The address book is small; this is the ceiling, not a page size. */
const val CONNECTION_PEOPLE_LIMIT = 200

/** Connection events read per reload. Ten are shown; the rest back `Show all activity`. */
const val CONNECTION_ACTIVITY_QUERY_LIMIT = 50

/** Events shown while Recent activity is not expanded to everything. */
const val CONNECTION_ACTIVITY_PREVIEW_COUNT = 10

/** Rows shown before Other people collapses behind a `Show N people` control. */
const val CONNECTION_OTHER_PEOPLE_COLLAPSE_AT = 5

/**
 * Recent events shown inside one person's expansion.
 *
 * The spec's number, and it is also why this query is not part of the page
 * reload: five rows for one person, read once when a reader asks for them, is
 * a bounded cost that does not multiply by the address book.
 */
const val CONNECTION_PERSON_EVENT_LIMIT = 5

/**
 * Turn live signals plus the last store snapshot into everything the page
 * renders.
 *
 * All three classifications come from the core and none is second-guessed
 * here. In particular the relay state the health card reports is the
 * *normalized* one from the core's evidence, and the Paths row renders that
 * same value -- which is what stops the page claiming Shore Pass is connected
 * on a phone that has been offline for an hour.
 *
 * The order below is load-bearing. Each person's delivery is classified
 * *before* the grouping call, and the attention it produces is what the
 * grouping is given. That is what makes a Needs attention row and its own
 * delivery line the same verdict rather than two judgements that can disagree
 * -- a person cannot be filed under a problem their row does not state.
 */
@Suppress("LongParameterList")
fun buildConnectionDetailsState(
    runtimeState: MeshRuntimeState,
    transports: Map<String, MeshRouterState.Transport>,
    relayHealth: RelayHealth,
    relayConfigured: Boolean,
    lanListening: Boolean,
    bluetoothAudioActive: Boolean,
    presenceLastSeen: Map<String, Long>,
    contactLastSeen: Map<String, Long>,
    snapshot: ConnectionStoreSnapshot,
    checkingSinceMs: Long,
    refreshing: Boolean,
    nowMs: Long,
): ConnectionDetailsState {
    val people = snapshot.people
    // Only friends count as "nearby": a stranger's phone HELLO'ing past is not
    // someone this page can promise anything about. Blocked identities are not
    // friends either -- a block is a tombstone, and a count that only a blocked
    // person produces ("1 friend nearby" above a People section with nobody in
    // it) discloses their presence just as surely as a row would.
    val visibleHexes = people.asSequence().filter { !it.blocked }.map { it.userIdHex }.toSet()
    val friendTransports = transports.filterKeys { hex -> hex in visibleHexes }
    val bluetoothLinks = friendTransports.count { it.value != MeshRouterState.Transport.LAN }
    val localWifiLinks = friendTransports.count { it.value == MeshRouterState.Transport.LAN }

    val runtime = ConnectionInputs.runtime(runtimeState)
    val relayPath = ConnectionInputs.relay(relayHealth, relayConfigured)
    val report = coreClassifyConnectionHealth(
        CoreConnectionHealthInput(
            runtime = runtime,
            bluetooth = ConnectionInputs.bluetooth(runtimeState),
            bluetoothLinks = bluetoothLinks.toUInt(),
            localWifi = ConnectionInputs.localWifi(runtimeState, lanListening),
            localWifiLinks = localWifiLinks.toUInt(),
            relay = relayPath,
            validatedInternet = ConnectionInputs.validatedInternet(relayHealth),
            nearbyFriendCount = friendTransports.size.toUInt(),
            checkingSinceMs = checkingSinceMs,
            nowMs = nowMs,
        ),
    )
    val evidence = report.evidence

    // Classified once per person per reload, and reused for the grouping, the
    // row, and the expansion -- not recomputed at each of those points, which
    // would be three FFI calls per friend on a list that recomposes whenever
    // any observable moves.
    val deliveryByHex = people.associate { person ->
        val directLink = ConnectionInputs.directLink(transports[person.userIdHex])
        person.userIdHex to DeliveryPresentation.line(
            person = person,
            directLink = directLink != null,
            ownRelayUsable = evidence.ownRelayUsable,
            relay = evidence.relay,
            nowMs = nowMs,
        )
    }

    val placements = coreGroupPeople(
        people.map { person ->
            val delivery = deliveryByHex[person.userIdHex]
            CorePersonHealthInput(
                userId = person.userId,
                displayName = person.name,
                blocked = person.blocked,
                directLink = ConnectionInputs.directLink(transports[person.userIdHex]),
                presenceLastSeenMs = presenceLastSeen[person.userIdHex] ?: 0L,
                lastSeenMs = maxOf(
                    contactLastSeen[person.userIdHex] ?: 0L,
                    person.latest?.atMs ?: 0L,
                ),
                attention = delivery?.attention,
                attentionSinceMs = delivery?.oldestWaitingMs ?: 0L,
            )
        },
        evidence.ownRelayUsable,
        nowMs,
    )

    val byHex = people.associateBy { it.userIdHex }
    fun rowsFor(group: List<uniffi.cruisemesh_core.CorePersonPlacement>): List<ConnectionPersonRow> =
        group.mapNotNull { placement ->
            val person = byHex[UserIdHex.encode(placement.userId)] ?: return@mapNotNull null
            personRow(
                person = person,
                reach = placement.reach,
                presenceLastSeenMs = presenceLastSeen[person.userIdHex] ?: 0L,
                delivery = deliveryByHex[person.userIdHex],
                bestRoute = DeliveryPresentation.bestRoute(
                    person = person,
                    directLink = ConnectionInputs.directLink(transports[person.userIdHex]),
                    ownRelayUsable = evidence.ownRelayUsable,
                    nowMs = nowMs,
                ),
                lastSeenMs = maxOf(
                    contactLastSeen[person.userIdHex] ?: 0L,
                    presenceLastSeen[person.userIdHex] ?: 0L,
                    person.latest?.atMs ?: 0L,
                ),
            )
        }

    return ConnectionDetailsState(
        health = HealthCardState(
            state = report.state,
            nearbyFriendCount = friendTransports.size,
            bluetooth = evidence.bluetooth,
            relay = evidence.relay,
            reason = report.reason,
            action = report.action,
        ),
        paths = PathsCardState(
            bluetooth = evidence.bluetooth,
            bluetoothLinks = bluetoothLinks,
            bluetoothAudioActive = bluetoothAudioActive,
            localWifiLinks = localWifiLinks,
            relay = evidence.relay,
            relayLastSyncMs = ConnectionInputs.relayLastSyncMs(relayHealth),
        ),
        needsAttention = rowsFor(placements.needsAttention),
        reachableNow = rowsFor(placements.reachableNow),
        otherPeople = rowsFor(placements.otherPeople),
        hasContacts = people.any { !it.blocked },
        activity = snapshot.activity,
        updatedAtMs = snapshot.loadedAtMs,
        refreshing = refreshing,
    )
}

@Suppress("LongParameterList")
private fun personRow(
    person: ConnectionPerson,
    reach: CorePersonReach,
    presenceLastSeenMs: Long,
    delivery: CoreDeliveryLine?,
    bestRoute: CorePersonRoute,
    lastSeenMs: Long,
): ConnectionPersonRow {
    val status: PersonStatus
    val badge: ConnectionPathBadge?
    when (reach) {
        CorePersonReach.DIRECT_BLUETOOTH -> {
            status = PersonStatus.ConnectedNow
            badge = ConnectionPathBadge.BLUETOOTH
        }
        CorePersonReach.DIRECT_LOCAL_WIFI -> {
            status = PersonStatus.ConnectedNow
            badge = ConnectionPathBadge.LOCAL_WIFI
        }
        CorePersonReach.RELAY_PRESENCE -> {
            status = PersonStatus.SeenOnline(presenceLastSeenMs)
            badge = ConnectionPathBadge.SHORE_PASS
        }
        CorePersonReach.NONE -> {
            val latest = person.latest
            status = if (latest == null) {
                PersonStatus.NoHistory
            } else {
                PersonStatus.History(latest.evidence, latest.atMs)
            }
            badge = latest?.let { badgeFor(it.transport) }
        }
    }
    return ConnectionPersonRow(
        userIdHex = person.userIdHex,
        name = person.name,
        status = status,
        badge = badge,
        delivery = delivery,
        attention = delivery?.attention,
        detail = PersonDetail(
            bestRoute = bestRoute,
            lastSeenMs = lastSeenMs,
            lastDeliveredMs = person.lastDeliveredMs,
        ),
    )
}

/**
 * The badge for an observed path, or null when no path was observed.
 *
 * Null exactly for a carried arrival: another phone brought the message the
 * last hop, so naming a radio here would claim the friend was in range when
 * they may be nowhere near.
 */
fun badgeFor(transport: PeerConnectionTransport): ConnectionPathBadge? = when (transport) {
    PeerConnectionTransport.BLUETOOTH -> ConnectionPathBadge.BLUETOOTH
    PeerConnectionTransport.LOCAL_WIFI -> ConnectionPathBadge.LOCAL_WIFI
    PeerConnectionTransport.SHORE_PASS -> ConnectionPathBadge.SHORE_PASS
    PeerConnectionTransport.CARRIED -> null
}

/** The one button a How-to-fix sheet may carry, if it carries one at all. */
enum class HowToFixAction {
    /**
     * No button. Nothing the app can open repairs a friend's card, an
     * oversized message, or a full mailbox, and a button that leads somewhere
     * useless costs a reader more than no button at all.
     */
    NONE,

    /** Opens the Shore Pass screen, where this phone's own setup is managed. */
    MANAGE_SHORE_PASS,

    /**
     * Opens the renewal page in the browser.
     *
     * Only an expired pass gets this, and it gets it *instead of* the pass
     * screen: nothing on that screen can renew, so sending someone there was
     * the dead end this replaces. A suspended pass is deliberately not here --
     * paying again does not lift a suspension.
     */
    RENEW_SHORE_PASS,
}

/** The How-to-fix button for a fault stopping delivery to one friend. */
fun howToFixAction(reason: CoreDeliveryBlockedReason): HowToFixAction = when (reason) {
    CoreDeliveryBlockedReason.PASS_EXPIRED -> HowToFixAction.RENEW_SHORE_PASS
    CoreDeliveryBlockedReason.PASS_SUSPENDED,
    CoreDeliveryBlockedReason.OWN_SETUP_REJECTED,
    -> HowToFixAction.MANAGE_SHORE_PASS
    CoreDeliveryBlockedReason.CONTACT_SETUP_REJECTED,
    CoreDeliveryBlockedReason.STORAGE_FULL,
    CoreDeliveryBlockedReason.MESSAGE_TOO_LARGE,
    -> HowToFixAction.NONE
}

/** The How-to-fix button for a device-wide fault. */
fun howToFixAction(reason: CoreHealthReason): HowToFixAction = when (reason) {
    CoreHealthReason.PASS_EXPIRED -> HowToFixAction.RENEW_SHORE_PASS
    CoreHealthReason.PASS_SUSPENDED,
    CoreHealthReason.OWN_SETUP_REJECTED,
    -> HowToFixAction.MANAGE_SHORE_PASS
    else -> HowToFixAction.NONE
}
