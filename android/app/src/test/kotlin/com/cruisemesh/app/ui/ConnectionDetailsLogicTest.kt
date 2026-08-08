package com.cruisemesh.app.ui

import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.mesh.MeshRouterState
import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreConnectionHealth
import uniffi.cruisemesh_core.CoreDeliveryState
import uniffi.cruisemesh_core.CoreDirectLink
import uniffi.cruisemesh_core.CoreDirectPathState
import uniffi.cruisemesh_core.CoreHealthAction
import uniffi.cruisemesh_core.CoreHealthReason
import uniffi.cruisemesh_core.CoreMeshRuntime
import uniffi.cruisemesh_core.CoreRelayPathState
import uniffi.cruisemesh_core.PeerConnectionTransport
import java.util.concurrent.atomic.AtomicInteger

/**
 * The page's shell-side logic, tested without Compose, Android, or a store.
 *
 * The interpretation itself is the core's and is tested in Rust; what is
 * pinned here is the join -- that this platform's signals reach the core
 * unmangled, and that the answer comes back out as rows that say what the
 * spec's examples say.
 */
class ConnectionDetailsLogicTest {

    private companion object {
        const val NOW = 1_760_000_000_000L
        const val START_OF_TODAY = NOW - 10 * ConnectionTimes.HOUR_MS
        const val MINUTE = ConnectionTimes.MINUTE_MS
        const val HOUR = ConnectionTimes.HOUR_MS
        const val DAY = ConnectionTimes.DAY_MS
    }

    // -----------------------------------------------------------------------
    // Signal mapping
    // -----------------------------------------------------------------------

    @Test
    fun `runtime states map one for one`() {
        assertEquals(CoreMeshRuntime.STOPPED, ConnectionInputs.runtime(MeshRuntimeState.STOPPED))
        assertEquals(CoreMeshRuntime.STARTING, ConnectionInputs.runtime(MeshRuntimeState.STARTING))
        assertEquals(CoreMeshRuntime.ACTIVE, ConnectionInputs.runtime(MeshRuntimeState.ACTIVE))
        assertEquals(
            CoreMeshRuntime.BLUETOOTH_OFF,
            ConnectionInputs.runtime(MeshRuntimeState.NO_BLUETOOTH),
        )
    }

    @Test
    fun `a runtime that reports Bluetooth off is a Bluetooth path that is off`() {
        assertEquals(
            CoreDirectPathState.OFF,
            ConnectionInputs.bluetooth(MeshRuntimeState.NO_BLUETOOTH),
        )
        assertEquals(
            CoreDirectPathState.AVAILABLE,
            ConnectionInputs.bluetooth(MeshRuntimeState.ACTIVE),
        )
        assertEquals(
            CoreDirectPathState.STARTING,
            ConnectionInputs.bluetooth(MeshRuntimeState.STARTING),
        )
    }

    @Test
    fun `local Wi-Fi follows the listening socket, not the service`() {
        // The service can be up on a phone with Wi-Fi off; saying the path is
        // available there is the "zombie header" failure class.
        assertEquals(
            CoreDirectPathState.OFF,
            ConnectionInputs.localWifi(MeshRuntimeState.ACTIVE, listening = false),
        )
        assertEquals(
            CoreDirectPathState.AVAILABLE,
            ConnectionInputs.localWifi(MeshRuntimeState.ACTIVE, listening = true),
        )
        assertEquals(
            CoreDirectPathState.OFF,
            ConnectionInputs.localWifi(MeshRuntimeState.STOPPED, listening = true),
        )
    }

    @Test
    fun `every relay health has exactly one path state`() {
        val cases = listOf(
            // With a pass saved, "no verdict published" is Checking, never
            // "not set up": the service publishes NoConfig before its first
            // check lands and again after it tears its status down, and
            // telling someone who has a pass to go and set one up is the one
            // answer that is certainly wrong.
            RelayHealth.NoConfig to CoreRelayPathState.CHECKING,
            RelayHealth.Checking to CoreRelayPathState.CHECKING,
            RelayHealth.NoInternet to CoreRelayPathState.WAITING_FOR_INTERNET,
            RelayHealth.Ok(NOW) to CoreRelayPathState.CONNECTED,
            RelayHealth.Failing(NOW) to CoreRelayPathState.UNREACHABLE,
            RelayHealth.Expired(NOW) to CoreRelayPathState.PASS_EXPIRED,
            RelayHealth.Suspended(NOW) to CoreRelayPathState.PASS_SUSPENDED,
            RelayHealth.TokenRejected(NOW) to CoreRelayPathState.SETUP_REJECTED,
            RelayHealth.QuotaFull(NOW) to CoreRelayPathState.STORAGE_FULL,
            RelayHealth.RateLimited(NOW) to CoreRelayPathState.SYNCING_SLOWED,
        )
        for ((health, expected) in cases) {
            assertEquals(health.toString(), expected, ConnectionInputs.relay(health, true))
        }
    }

    @Test
    fun `an oversized message is not a broken pass`() {
        // Per the spec, "message too large" is a fact about one message and one
        // recipient. The service is reachable and everything else still moves.
        assertEquals(
            CoreRelayPathState.CONNECTED,
            ConnectionInputs.relay(RelayHealth.MessageTooLarge(NOW), true),
        )
    }

    @Test
    fun `no saved pass is not set up whatever the last health said`() {
        assertEquals(
            CoreRelayPathState.NOT_SET_UP,
            ConnectionInputs.relay(RelayHealth.Ok(NOW), configured = false),
        )
    }

    @Test
    fun `only the no-internet verdict means no validated internet`() {
        assertFalse(ConnectionInputs.validatedInternet(RelayHealth.NoInternet))
        assertTrue(ConnectionInputs.validatedInternet(RelayHealth.Ok(NOW)))
        assertTrue(ConnectionInputs.validatedInternet(RelayHealth.Failing(NOW)))
    }

    @Test
    fun `both Bluetooth roles are one Bluetooth link`() {
        assertEquals(
            CoreDirectLink.BLUETOOTH,
            ConnectionInputs.directLink(MeshRouterState.Transport.CENTRAL),
        )
        assertEquals(
            CoreDirectLink.BLUETOOTH,
            ConnectionInputs.directLink(MeshRouterState.Transport.PERIPHERAL),
        )
        assertEquals(
            CoreDirectLink.LOCAL_WIFI,
            ConnectionInputs.directLink(MeshRouterState.Transport.LAN),
        )
        assertNull(ConnectionInputs.directLink(null))
    }

    @Test
    fun `a carried arrival names no path`() {
        assertNull(badgeFor(PeerConnectionTransport.CARRIED))
        assertEquals(ConnectionPathBadge.BLUETOOTH, badgeFor(PeerConnectionTransport.BLUETOOTH))
        assertEquals(ConnectionPathBadge.LOCAL_WIFI, badgeFor(PeerConnectionTransport.LOCAL_WIFI))
        assertEquals(ConnectionPathBadge.SHORE_PASS, badgeFor(PeerConnectionTransport.SHORE_PASS))
    }

    // -----------------------------------------------------------------------
    // Checking clock
    // -----------------------------------------------------------------------

    @Test
    fun `a pending check keeps its original start mark`() {
        val clock = CheckingClock()
        assertEquals(NOW, clock.mark(pending = true, nowMs = NOW))
        // A recomposition three seconds later must not restart the bound, or
        // the card can never leave Checking.
        assertEquals(NOW, clock.mark(pending = true, nowMs = NOW + 3_000))
        assertEquals(0L, clock.mark(pending = false, nowMs = NOW + 4_000))
        assertEquals(NOW + 5_000, clock.mark(pending = true, nowMs = NOW + 5_000))
    }

    @Test
    fun `every path that can still be coming up counts as pending`() {
        // Including the two radios: a shell that only watched the runtime and
        // the pass would never start the bound for a radio that has not
        // answered yet, and would render a failure while it was still
        // answering.
        assertTrue(
            connectionCheckPending(
                CoreMeshRuntime.STARTING,
                CoreDirectPathState.AVAILABLE,
                CoreDirectPathState.AVAILABLE,
                CoreRelayPathState.CONNECTED,
            ),
        )
        assertTrue(
            connectionCheckPending(
                CoreMeshRuntime.ACTIVE,
                CoreDirectPathState.STARTING,
                CoreDirectPathState.OFF,
                CoreRelayPathState.NOT_SET_UP,
            ),
        )
        assertTrue(
            connectionCheckPending(
                CoreMeshRuntime.ACTIVE,
                CoreDirectPathState.OFF,
                CoreDirectPathState.STARTING,
                CoreRelayPathState.NOT_SET_UP,
            ),
        )
        assertTrue(
            connectionCheckPending(
                CoreMeshRuntime.ACTIVE,
                CoreDirectPathState.AVAILABLE,
                CoreDirectPathState.AVAILABLE,
                CoreRelayPathState.CHECKING,
            ),
        )
        assertFalse(
            connectionCheckPending(
                CoreMeshRuntime.ACTIVE,
                CoreDirectPathState.AVAILABLE,
                CoreDirectPathState.AVAILABLE,
                CoreRelayPathState.CONNECTED,
            ),
        )
        assertFalse(
            connectionCheckPending(
                CoreMeshRuntime.STOPPED,
                CoreDirectPathState.OFF,
                CoreDirectPathState.OFF,
                CoreRelayPathState.NOT_SET_UP,
            ),
        )
    }

    // -----------------------------------------------------------------------
    // Freshness
    // -----------------------------------------------------------------------

    @Test
    fun `freshness buckets`() {
        assertEquals(FreshnessLabel.Never, ConnectionTimes.freshness(0L, NOW))
        assertEquals(FreshnessLabel.Never, ConnectionTimes.freshness(-1L, NOW))
        assertEquals(FreshnessLabel.JustNow, ConnectionTimes.freshness(NOW, NOW))
        assertEquals(FreshnessLabel.JustNow, ConnectionTimes.freshness(NOW - 59_000, NOW))
        assertEquals(FreshnessLabel.Minutes(1), ConnectionTimes.freshness(NOW - MINUTE, NOW))
        assertEquals(FreshnessLabel.Minutes(59), ConnectionTimes.freshness(NOW - 59 * MINUTE, NOW))
        assertEquals(FreshnessLabel.Hours(1), ConnectionTimes.freshness(NOW - HOUR, NOW))
        assertEquals(FreshnessLabel.Hours(5), ConnectionTimes.freshness(NOW - 5 * HOUR, NOW))
    }

    @Test
    fun `a snapshot stamped in the future reads as just now, never as a negative age`() {
        assertEquals(FreshnessLabel.JustNow, ConnectionTimes.freshness(NOW + 60_000, NOW))
    }

    // -----------------------------------------------------------------------
    // Event times
    // -----------------------------------------------------------------------

    @Test
    fun `a zero or negative timestamp is never a date`() {
        assertEquals(EventTime.Unknown, ConnectionTimes.eventTime(0L, NOW, START_OF_TODAY))
        assertEquals(EventTime.Unknown, ConnectionTimes.eventTime(-5L, NOW, START_OF_TODAY))
    }

    @Test
    fun `event time buckets`() {
        assertEquals(
            EventTime.JustNow,
            ConnectionTimes.eventTime(NOW - 30_000, NOW, START_OF_TODAY),
        )
        assertEquals(
            EventTime.Minutes(12),
            ConnectionTimes.eventTime(NOW - 12 * MINUTE, NOW, START_OF_TODAY),
        )
        assertEquals(
            EventTime.Hours(3),
            ConnectionTimes.eventTime(NOW - 3 * HOUR, NOW, START_OF_TODAY),
        )
        // Yesterday evening, seen this morning: the calendar day wins over the
        // raw age, which is the reading the spec's own example asks for.
        assertEquals(
            EventTime.Yesterday,
            ConnectionTimes.eventTime(START_OF_TODAY - 4 * HOUR, NOW, START_OF_TODAY),
        )
        assertEquals(
            EventTime.Older,
            ConnectionTimes.eventTime(START_OF_TODAY - DAY - 1, NOW, START_OF_TODAY),
        )
    }

    // -----------------------------------------------------------------------
    // Coalescing
    // -----------------------------------------------------------------------

    @Test
    fun `a burst of signals inside the window costs exactly one reload`() {
        val coalescer = StoreChangeCoalescer(windowMs = 500L)
        var clock = NOW
        assertTrue(coalescer.onSignal(clock))
        // A thousand more events over the next 400 ms -- the mesh-flood case.
        repeat(1_000) {
            clock += 0
            assertFalse(coalescer.onSignal(clock + 400))
        }
        assertEquals(500L, coalescer.remainingMs(NOW))
        assertEquals(100L, coalescer.remainingMs(NOW + 400))
        assertEquals(0L, coalescer.remainingMs(NOW + 500))
    }

    @Test
    fun `a signal arriving mid-reload schedules exactly one follow-up`() {
        val coalescer = StoreChangeCoalescer(windowMs = 500L)
        assertTrue(coalescer.onSignal(NOW))
        coalescer.onReloadStarted()
        assertFalse(coalescer.onSignal(NOW + 10))
        assertFalse(coalescer.onSignal(NOW + 20))
        assertFalse(coalescer.onSignal(NOW + 30))
        assertTrue(coalescer.onReloadFinished())

        // Exactly one: the follow-up is not owed twice.
        assertTrue(coalescer.onSignal(NOW + 40))
        coalescer.onReloadStarted()
        assertFalse(coalescer.onReloadFinished())
    }

    @Test
    fun `signals during the wait do not extend the window`() {
        val coalescer = StoreChangeCoalescer(windowMs = 500L)
        assertTrue(coalescer.onSignal(NOW))
        assertFalse(coalescer.onSignal(NOW + 490))
        assertEquals(0L, coalescer.remainingMs(NOW + 500))
    }

    @Test
    fun `a backwards clock cannot stall the page behind a huge wait`() {
        val coalescer = StoreChangeCoalescer(windowMs = 500L)
        assertTrue(coalescer.onSignal(NOW))
        // The clock jumps an hour into the past under us.
        assertEquals(500L, coalescer.remainingMs(NOW - HOUR))
    }

    @Test
    fun `nothing is owed before anything is signalled`() {
        val coalescer = StoreChangeCoalescer()
        assertEquals(0L, coalescer.remainingMs(NOW))
    }

    // -----------------------------------------------------------------------
    // The reload loop
    //
    // The policy above passes with the loop wired up wrongly -- everything
    // that can wedge this page lives in the seam between the two, and the seam
    // only exists while a loop is actually running and being cancelled. So
    // these drive the real loop, with real signals and a real pause.
    // -----------------------------------------------------------------------

    /** One run of the loop, with the levers a test needs to hold. */
    private class LoopHarness {
        val coalescer = StoreChangeCoalescer()
        val requests = Channel<Unit>(Channel.CONFLATED)
        val loads = AtomicInteger(0)
        /** Set to make the next load hang until the job is cancelled. */
        @Volatile
        var blockLoad = false

        fun signal() {
            if (coalescer.onSignal(System.currentTimeMillis())) requests.trySend(Unit)
        }

        fun CoroutineScope.start(): Job = launch {
            runConnectionRefreshLoop(
                coalescer = coalescer,
                requests = requests,
                signal = ::signal,
                nowMs = System::currentTimeMillis,
                // Long enough that nothing in these tests is driven by the
                // poll: every reload here is one a test asked for.
                pollIntervalMs = 60_000L,
                onRefreshingChanged = {},
                load = {
                    if (blockLoad) awaitCancellation()
                    loads.incrementAndGet()
                    ConnectionStoreSnapshot(emptyList(), emptyList(), System.currentTimeMillis())
                },
                onLoaded = {},
            )
        }

        suspend fun awaitLoads(count: Int) {
            withTimeout(5_000L) {
                while (loads.get() < count) delay(10L)
            }
        }
    }

    @Test
    fun `the loop loads once as soon as it starts, without waiting out a debounce`() = runBlocking {
        val harness = LoopHarness()
        val job = with(harness) { start() }
        harness.awaitLoads(1)
        job.cancelAndJoin()
    }

    @Test
    fun `a pause inside the coalescing window does not wedge the next run`() = runBlocking {
        val harness = LoopHarness()
        val first = with(harness) { start() }
        harness.awaitLoads(1)

        // A store change opens a 500 ms window; the loop takes the request and
        // waits it out. The screen locks part-way through.
        harness.signal()
        delay(100L)
        first.cancelAndJoin()

        // ON_RESUME. Without the reset the coalescer still believes a window is
        // open, absorbs the seed, and the loop blocks on an empty channel
        // forever: rows frozen for the life of the composition.
        val second = with(harness) { start() }
        harness.awaitLoads(2)
        second.cancelAndJoin()
    }

    @Test
    fun `a pause during a load does not wedge the next run`() = runBlocking {
        val harness = LoopHarness()
        harness.blockLoad = true
        val first = with(harness) { start() }
        // Give the loop time to reach the load and hang there.
        delay(200L)
        assertEquals(0, harness.loads.get())
        first.cancelAndJoin()

        harness.blockLoad = false
        val second = with(harness) { start() }
        harness.awaitLoads(1)
        second.cancelAndJoin()
    }

    @Test
    fun `a burst of signals through the running loop costs one reload, then one follow-up`() =
        runBlocking {
            val harness = LoopHarness()
            val job = with(harness) { start() }
            harness.awaitLoads(1)

            // A thousand store events inside one window -- the mesh-flood case,
            // pumped through the real loop rather than the policy object.
            repeat(1_000) { harness.signal() }
            harness.awaitLoads(2)
            // Nothing has been signalled since, so no further reload is owed.
            delay(CONNECTION_COALESCE_WINDOW_MS * 2)
            assertEquals(2, harness.loads.get())
            job.cancelAndJoin()
        }

    // -----------------------------------------------------------------------
    // Delivery language
    // -----------------------------------------------------------------------

    @Suppress("LongParameterList")
    private fun deliveryLine(
        queued: Int,
        routeIsDirect: Boolean = false,
        ownRelayUsable: Boolean = true,
        contactHasRelayEndpoint: Boolean = true,
        contactRelayStale: Boolean = false,
        relay: CoreRelayPathState = CoreRelayPathState.CONNECTED,
        receiptIsNewestEvidence: Boolean = false,
    ) = DeliveryPresentation.line(
        queued = queued,
        routeIsDirect = routeIsDirect,
        ownRelayUsable = ownRelayUsable,
        contactHasRelayEndpoint = contactHasRelayEndpoint,
        contactRelayStale = contactRelayStale,
        relay = relay,
        receiptIsNewestEvidence = receiptIsNewestEvidence,
    )

    @Test
    fun `nothing waiting means no delivery line at all`() {
        assertNull(deliveryLine(queued = 0))
    }

    @Test
    fun `a live link means the work is going out now`() {
        assertEquals(
            DeliveryLine(CoreDeliveryState.SENDING, 2),
            deliveryLine(queued = 2, routeIsDirect = true, ownRelayUsable = false),
        )
    }

    @Test
    fun `a working pass plus their endpoint is also a usable route`() {
        assertEquals(DeliveryLine(CoreDeliveryState.SENDING, 1), deliveryLine(queued = 1))
    }

    @Test
    fun `their written-off endpoint means no line, because the count cannot drain`() {
        // Nothing will ever mark these rows uploaded, so the number is not a
        // backlog: it is every message written to them inside the retention
        // window, and it would sit under their name for a week.
        assertNull(deliveryLine(queued = 4, contactRelayStale = true))
    }

    @Test
    fun `no internet with only a pass route says so plainly`() {
        assertEquals(
            DeliveryLine(CoreDeliveryState.WAITING_FOR_INTERNET, 3),
            deliveryLine(
                queued = 3,
                ownRelayUsable = false,
                relay = CoreRelayPathState.WAITING_FOR_INTERNET,
            ),
        )
    }

    @Test
    fun `a pass fault still promises the next encounter rather than a failure`() {
        assertEquals(
            DeliveryLine(CoreDeliveryState.WILL_DELIVER_WHEN_RECONNECTED, 4),
            deliveryLine(
                queued = 4,
                ownRelayUsable = false,
                relay = CoreRelayPathState.PASS_EXPIRED,
            ),
        )
    }

    @Test
    fun `a backlog that relay upload cannot drain is not shown at all`() {
        // The contradiction this page exists to remove. The count is rows
        // whose upload stamp is unset, and only an upload sets it -- not a
        // receipt, and not handing the message over in person. On a phone with
        // no pass saved, or for a friend whose card carries no endpoint, the
        // number never goes down.
        assertNull(
            deliveryLine(
                queued = 12,
                routeIsDirect = true,
                ownRelayUsable = false,
                relay = CoreRelayPathState.NOT_SET_UP,
            ),
        )
        assertNull(deliveryLine(queued = 12, contactHasRelayEndpoint = false))
    }

    @Test
    fun `a row that already says they received a message gets no line under it`() {
        assertNull(deliveryLine(queued = 12, receiptIsNewestEvidence = true))
    }

    // -----------------------------------------------------------------------
    // View-state assembly
    // -----------------------------------------------------------------------

    private fun person(
        id: Byte,
        name: String,
        blocked: Boolean = false,
        hasRelayEndpoint: Boolean = true,
        queued: Int = 0,
        latest: PeerStatusLine? = null,
    ): ConnectionPerson {
        val bytes = byteArrayOf(id)
        return ConnectionPerson(
            userIdHex = UserIdHex.encode(bytes),
            userId = bytes,
            name = name,
            blocked = blocked,
            hasRelayEndpoint = hasRelayEndpoint,
            queued = queued,
            latest = latest,
        )
    }

    private fun hex(id: Byte) = UserIdHex.encode(byteArrayOf(id))

    @Suppress("LongParameterList")
    private fun state(
        people: List<ConnectionPerson>,
        transports: Map<String, MeshRouterState.Transport> = emptyMap(),
        relayHealth: RelayHealth = RelayHealth.Ok(NOW),
        relayConfigured: Boolean = true,
        lanListening: Boolean = true,
        stale: Set<String> = emptySet(),
        presence: Map<String, Long> = emptyMap(),
        runtime: MeshRuntimeState = MeshRuntimeState.ACTIVE,
        activity: List<ConnectionActivityRow> = emptyList(),
    ) = buildConnectionDetailsState(
        runtimeState = runtime,
        transports = transports,
        relayHealth = relayHealth,
        relayConfigured = relayConfigured,
        lanListening = lanListening,
        bluetoothAudioActive = false,
        staleRelayContacts = stale,
        presenceLastSeen = presence,
        contactLastSeen = emptyMap(),
        snapshot = ConnectionStoreSnapshot(people, activity, NOW),
        checkingSinceMs = 0L,
        refreshing = false,
        nowMs = NOW,
    )

    @Test
    fun `a quiet phone with nobody nearby is working normally`() {
        val result = state(people = listOf(person(1, "Ash")))
        assertEquals(CoreConnectionHealth.READY, result.health.state)
        assertNull(result.health.reason)
        assertNull(result.health.action)
        assertEquals(0, result.health.nearbyFriendCount)
        assertTrue(result.reachableNow.isEmpty())
        assertEquals(listOf("Ash"), result.otherPeople.map { it.name })
    }

    @Test
    fun `a live link puts the friend in Reachable now with the right badge`() {
        val result = state(
            people = listOf(person(1, "Riley's phone"), person(2, "Sam")),
            transports = mapOf(
                hex(1) to MeshRouterState.Transport.LAN,
                hex(2) to MeshRouterState.Transport.CENTRAL,
            ),
        )
        assertEquals(listOf("Riley's phone", "Sam"), result.reachableNow.map { it.name })
        assertEquals(ConnectionPathBadge.LOCAL_WIFI, result.reachableNow[0].badge)
        assertEquals(ConnectionPathBadge.BLUETOOTH, result.reachableNow[1].badge)
        assertEquals(PersonStatus.ConnectedNow, result.reachableNow[0].status)
        assertEquals(2, result.health.nearbyFriendCount)
        assertEquals(1, result.paths.localWifiLinks)
        assertEquals(1, result.paths.bluetoothLinks)
    }

    @Test
    fun `a stranger nearby is not a friend nearby`() {
        val result = state(
            people = listOf(person(1, "Ash")),
            transports = mapOf("deadbeef" to MeshRouterState.Transport.CENTRAL),
        )
        assertEquals(0, result.health.nearbyFriendCount)
        assertEquals(0, result.paths.bluetoothLinks)
    }

    @Test
    fun `fresh presence with a working pass reads as seen online`() {
        // Inside the core's presence window; a stamp older than that is
        // evidence they *were* around, not that they are reachable now.
        val seenAt = NOW - 2 * MINUTE
        val result = state(
            people = listOf(person(1, "Ash")),
            presence = mapOf(hex(1) to seenAt),
        )
        assertEquals(listOf("Ash"), result.reachableNow.map { it.name })
        assertEquals(ConnectionPathBadge.SHORE_PASS, result.reachableNow[0].badge)
        assertEquals(PersonStatus.SeenOnline(seenAt), result.reachableNow[0].status)

        val stale = state(
            people = listOf(person(1, "Ash")),
            presence = mapOf(hex(1) to NOW - 10 * MINUTE),
        )
        assertTrue(stale.reachableNow.isEmpty())
    }

    @Test
    fun `Shore Pass connected without internet never claims to be connected`() {
        // The old page's flagship contradiction, in both places it could show.
        val result = state(
            people = listOf(person(1, "Ash")),
            relayHealth = RelayHealth.NoInternet,
            presence = mapOf(hex(1) to NOW - MINUTE),
        )
        assertEquals(CoreRelayPathState.WAITING_FOR_INTERNET, result.health.relay)
        assertEquals(CoreRelayPathState.WAITING_FOR_INTERNET, result.paths.relay)
        // And a friend seen a minute ago is not promised over a path this
        // phone does not have.
        assertTrue(result.reachableNow.isEmpty())
        assertEquals(listOf("Ash"), result.otherPeople.map { it.name })
    }

    @Test
    fun `a blocked friend appears in no group and in no activity`() {
        val result = state(
            people = listOf(
                person(9, "Blocked", blocked = true),
                person(2, "Sam"),
            ),
            transports = mapOf(hex(9) to MeshRouterState.Transport.CENTRAL),
        )
        val allNames = (result.reachableNow + result.otherPeople).map { it.name }
        assertFalse(allNames.contains("Blocked"))
        assertEquals(listOf("Sam"), allNames)
    }

    @Test
    fun `a friend with no history says so instead of inventing a date`() {
        val result = state(people = listOf(person(1, "Dana")))
        assertEquals(PersonStatus.NoHistory, result.otherPeople[0].status)
        assertNull(result.otherPeople[0].badge)
    }

    @Test
    fun `the newest evidence becomes the status sentence and its badge`() {
        val result = state(
            people = listOf(
                person(
                    1,
                    "Ash",
                    latest = PeerStatusLine(
                        PeerEvidence.MESSAGE_DELIVERED,
                        PeerConnectionTransport.SHORE_PASS,
                        NOW - 12 * MINUTE,
                    ),
                ),
            ),
        )
        assertEquals(
            PersonStatus.History(PeerEvidence.MESSAGE_DELIVERED, NOW - 12 * MINUTE),
            result.otherPeople[0].status,
        )
        assertEquals(ConnectionPathBadge.SHORE_PASS, result.otherPeople[0].badge)
    }

    @Test
    fun `waiting work never renders as an error, whatever the path state`() {
        val result = state(
            people = listOf(person(1, "Ash", queued = 3)),
            relayHealth = RelayHealth.NoInternet,
        )
        val delivery = result.otherPeople[0].delivery
        assertNotNull(delivery)
        assertEquals(CoreDeliveryState.WAITING_FOR_INTERNET, delivery?.kind)
        assertEquals(3, delivery?.count)
    }

    @Test
    fun `a friend who already received a message gets no queue line under the row`() {
        // The contradiction in one assertion: the row says "Received your
        // message 12 min ago" and the old page put "Sending 12 messages…"
        // directly beneath it, for as long as the retention window lasted.
        val result = state(
            people = listOf(
                person(
                    1,
                    "Ash",
                    queued = 12,
                    latest = PeerStatusLine(
                        PeerEvidence.MESSAGE_DELIVERED,
                        PeerConnectionTransport.SHORE_PASS,
                        NOW - 12 * MINUTE,
                    ),
                ),
            ),
        )
        assertEquals(
            PersonStatus.History(PeerEvidence.MESSAGE_DELIVERED, NOW - 12 * MINUTE),
            result.otherPeople[0].status,
        )
        assertNull(result.otherPeople[0].delivery)
    }

    @Test
    fun `a blocked friend standing next to us is counted nowhere`() {
        val result = state(
            people = listOf(person(1, "Ash", blocked = true), person(2, "Bo")),
            transports = mapOf(hex(1) to MeshRouterState.Transport.CENTRAL),
        )
        // Not in a group, and not in the numbers above the groups either: a
        // count only a blocked person produces discloses them just as surely.
        assertTrue(result.reachableNow.isEmpty())
        assertEquals(listOf("Bo"), result.otherPeople.map { it.name })
        assertEquals(0, result.health.nearbyFriendCount)
        assertEquals(0, result.paths.bluetoothLinks)
    }

    @Test
    fun `a stopped mesh needs attention and offers to start`() {
        val result = state(people = listOf(person(1, "Ash")), runtime = MeshRuntimeState.STOPPED)
        assertEquals(CoreConnectionHealth.NEEDS_ATTENTION, result.health.state)
        assertEquals(CoreHealthReason.MESH_STOPPED, result.health.reason)
        assertEquals(CoreHealthAction.START_MESH, result.health.action)
    }

    @Test
    fun `Bluetooth off with a working pass is limited, not broken`() {
        val result = state(
            people = listOf(person(1, "Ash")),
            runtime = MeshRuntimeState.NO_BLUETOOTH,
            lanListening = false,
        )
        assertEquals(CoreConnectionHealth.LIMITED, result.health.state)
        assertEquals(CoreHealthReason.BLUETOOTH_OFF, result.health.reason)
        assertEquals(CoreHealthAction.TURN_ON_BLUETOOTH, result.health.action)
        assertEquals(CoreDirectPathState.OFF, result.paths.bluetooth)
    }

    @Test
    fun `no saved pass is still working normally`() {
        val result = state(
            people = listOf(person(1, "Ash")),
            relayHealth = RelayHealth.NoConfig,
            relayConfigured = false,
        )
        assertEquals(CoreConnectionHealth.READY, result.health.state)
        assertEquals(CoreRelayPathState.NOT_SET_UP, result.paths.relay)
    }

    @Test
    fun `the last successful sync time comes through for the Paths row`() {
        val result = state(people = emptyList(), relayHealth = RelayHealth.Ok(NOW - 90_000))
        assertEquals(NOW - 90_000, result.paths.relayLastSyncMs)
        // Nothing to date on a health that never synced.
        val failing = state(people = emptyList(), relayHealth = RelayHealth.Failing(NOW))
        assertEquals(0L, failing.paths.relayLastSyncMs)
    }

    @Test
    fun `an empty address book is reported as empty, not as an empty group`() {
        val result = state(people = emptyList())
        assertFalse(result.hasContacts)
        assertTrue(result.reachableNow.isEmpty())
        assertTrue(result.otherPeople.isEmpty())
    }

    @Test
    fun `a page with only blocked friends has no contacts to show`() {
        val result = state(people = listOf(person(9, "Blocked", blocked = true)))
        assertFalse(result.hasContacts)
    }
}
