package com.cruisemesh.app.mesh

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CentralConnectAdmissionTest {
    // --- session / capacity machinery -------------------------------------

    @Test
    fun pendingConnectionsConsumeCapacityBeforeFrameworkCallsStart() {
        val admission = CentralConnectAdmission(maxActive = 2)
        admission.startSession()

        val first = admission.tryReserve("a", null, 0L).reservation
        val second = admission.tryReserve("b", null, 0L).reservation
        val denied = admission.tryReserve("c", null, 0L)

        assertNotNull(first)
        assertNotNull(second)
        assertNull(denied.reservation)
        assertTrue(denied.atCapacity)
        assertEquals(2, denied.activeCount)

        admission.cancel(second!!)
        assertNotNull(admission.tryReserve("c", null, 0L).reservation)
    }

    @Test
    fun stopInvalidatesQueuedAndInFlightReservations() {
        val admission = CentralConnectAdmission(maxActive = 2)
        admission.startSession()
        val queued = admission.tryReserve("a", null, 0L).reservation!!
        val inFlight = admission.tryReserve("b", null, 0L).reservation!!
        assertTrue(admission.beginConnect(inFlight))

        admission.stopSession()

        assertFalse(admission.beginConnect(queued))
        assertFalse(admission.completeConnect(inFlight))
        admission.startSession()
        assertNotNull(admission.tryReserve("a", null, 0L).reservation)
    }

    @Test
    fun concurrentScanCallbacksCannotOverReserveTheCap() {
        val admission = CentralConnectAdmission(maxActive = 5)
        admission.startSession()
        val start = CountDownLatch(1)
        val pool = Executors.newFixedThreadPool(16)
        try {
            val futures = (0 until 64).map { index ->
                pool.submit<CentralConnectAdmission.Reservation?> {
                    start.await()
                    admission.tryReserve("peer-$index", null, 0L).reservation
                }
            }
            start.countDown()
            assertEquals(5, futures.count { it.get() != null })
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun budgetIsNeverExceededAcrossMixedTraffic() {
        val standings = mutableMapOf<String, BlePeerStanding>()
        val admission = CentralConnectAdmission(
            maxActive = 3,
            selfInstanceToken = SELF,
            minHoldMs = 0L,
            standingOf = { standings[it] ?: BlePeerStanding.UNKNOWN },
        )
        admission.startSession()
        var now = 0L
        val live = mutableSetOf<String>()
        repeat(400) { step ->
            now += 1_000
            val address = "peer-${step % 37}"
            val token = if (step % 3 == 0) null else "tok-${step % 11}"
            val attempt = admission.tryReserve(address, if (step % 17 == 0) SELF else token, now)
            attempt.preemptedAddress?.let(live::remove)
            if (attempt.reservation != null) {
                live += address
                if (step % 5 != 0) {
                    val userId = "user-${step % 7}"
                    standings[userId] = BlePeerStanding.entries[step % 3]
                    admission.onIdentified(address, userId)?.let(live::remove)
                }
            }
            if (step % 4 == 0) live.toList().firstOrNull()?.let {
                live -= it
                admission.disconnect(it)
            }
            assertTrue("slot count ${admission.activeCount()} exceeded 3", admission.activeCount() <= 3)
        }
    }

    // --- identity dedupe --------------------------------------------------

    @Test
    fun oneAdvertisedIdentityNeverHoldsTwoSlots() {
        val admission = CentralConnectAdmission(maxActive = 4)
        admission.startSession()

        assertNotNull(admission.tryReserve("aa:01", TOKEN_A, 0L).reservation)
        // Same phone, rotated to a fresh resolvable private address.
        val rotated = admission.tryReserve("aa:02", TOKEN_A, 1_000L)

        assertNull(rotated.reservation)
        assertEquals(CentralConnectAdmission.Outcome.DUPLICATE_IDENTITY, rotated.outcome)
        assertEquals(1, admission.activeCount())
    }

    @Test
    fun rotatedAddressesOfOneAdvertiserShareABackoffKey() {
        val admission = CentralConnectAdmission(maxActive = 4)
        admission.startSession()

        assertEquals(
            admission.identityKeyFor("aa:01", TOKEN_A),
            admission.identityKeyFor("aa:02", TOKEN_A),
        )
        // No service data: nothing links the two addresses, and the key says so.
        assertTrue(admission.identityKeyFor("bb:01", null) != admission.identityKeyFor("bb:02", null))
    }

    @Test
    fun aLiveLinksKeyFollowsItsUpgradeToAUserId() {
        // The failure and success paths only ever have an address, so the key
        // they file under has to move with the link when HELLO names it --
        // otherwise a success clears one counter while the next scan sighting
        // of the same peer consults another.
        val admission = CentralConnectAdmission(maxActive = 4)
        admission.startSession()
        assertNotNull(admission.tryReserve("aa:01", TOKEN_A, 0L).reservation)

        assertEquals("adv:$TOKEN_A", admission.identityKeyOf("aa:01"))
        admission.onIdentified("aa:01", ALICE)

        assertEquals("user:$ALICE", admission.identityKeyOf("aa:01"))
        assertEquals(admission.identityKeyFor("aa:01", TOKEN_A), admission.identityKeyOf("aa:01"))
    }

    @Test
    fun anUntrackedAddressFallsBackToItsOwnKey() {
        val admission = CentralConnectAdmission(maxActive = 4)
        admission.startSession()

        assertEquals(admission.identityKeyFor("zz:99", null), admission.identityKeyOf("zz:99"))
    }

    @Test
    fun aTokenLearnsItsUserIdAndKeepsItAcrossRotation() {
        val admission = CentralConnectAdmission(maxActive = 4)
        admission.startSession()
        assertNotNull(admission.tryReserve("aa:01", TOKEN_A, 0L).reservation)
        admission.onIdentified("aa:01", ALICE)
        admission.disconnect("aa:01")

        assertEquals("user:$ALICE", admission.identityKeyFor("aa:99", TOKEN_A))
    }

    @Test
    fun tokenlessDuplicatesAreDroppedOnceHelloNamesThem() {
        val admission = CentralConnectAdmission(maxActive = 4)
        admission.startSession()
        // An iPhone advertises no service data, so its two rotated addresses
        // cannot be told apart until each has connected and said who it is.
        assertNotNull(admission.tryReserve("ios:01", null, 0L).reservation)
        assertNotNull(admission.tryReserve("ios:02", null, 5_000L).reservation)
        assertEquals(2, admission.activeCount())

        assertNull(admission.onIdentified("ios:01", ALICE))
        val redundant = admission.onIdentified("ios:02", ALICE)

        // The younger link is the one dropped; the older is the elected route.
        assertEquals("ios:02", redundant)
        assertEquals(1, admission.activeCount())
    }

    @Test
    fun theOlderLinkSurvivesEvenWhenTheYoungerIdentifiesFirst() {
        val admission = CentralConnectAdmission(maxActive = 4)
        admission.startSession()
        assertNotNull(admission.tryReserve("ios:01", null, 0L).reservation)
        assertNotNull(admission.tryReserve("ios:02", null, 5_000L).reservation)

        assertNull(admission.onIdentified("ios:02", ALICE))
        assertEquals("ios:02", admission.onIdentified("ios:01", ALICE))
    }

    // --- self -------------------------------------------------------------

    @Test
    fun ourOwnAdvertisementIsNeverSelected() {
        val admission = CentralConnectAdmission(maxActive = 4, selfInstanceToken = SELF)
        admission.startSession()

        val attempt = admission.tryReserve("aa:01", SELF, 0L)

        assertNull(attempt.reservation)
        assertEquals(CentralConnectAdmission.Outcome.SELF, attempt.outcome)
        assertEquals(0, admission.activeCount())
        // Still refused when nothing else is competing and the address rotates.
        assertNull(admission.tryReserve("aa:02", SELF, 60_000L).reservation)
    }

    @Test
    fun ourOwnListenerStaysRefusedOnTheHalfOfItsAdvertisementCarryingNoToken() {
        // The field bug: a legacy advertisement is two PDUs, and the bare
        // ADV_IND half arrives with no scan response merged in. The old guard
        // read the service data off the result in hand, saw none, and dialled
        // our own listener again every scan window.
        val admission = CentralConnectAdmission(maxActive = 4, selfInstanceToken = SELF)
        admission.startSession()
        assertEquals(CentralConnectAdmission.Outcome.SELF, admission.tryReserve("aa:01", SELF, 0L).outcome)

        val bare = admission.tryReserve("aa:01", null, 1_000L)

        assertNull(bare.reservation)
        assertEquals(CentralConnectAdmission.Outcome.SELF, bare.outcome)
        // And still refused after the role is stopped and started again --
        // our own advertisement outlives one scan session.
        admission.stopSession()
        admission.startSession()
        assertEquals(CentralConnectAdmission.Outcome.SELF, admission.tryReserve("aa:01", null, 2_000L).outcome)
    }

    @Test
    fun aSecondPackageOfThisAppIsAnOrdinaryPeer() {
        // Two builds side by side on one handset are a test rig, not a self
        // dial: the token is per process, so they hold distinct tokens and
        // must be free to link. This is why the guard can never be keyed on
        // anything both packages share -- such a value would be shared by
        // every phone running the app, and reject the whole mesh.
        val admission = CentralConnectAdmission(maxActive = 4, selfInstanceToken = SELF)
        admission.startSession()

        val attempt = admission.tryReserve("aa:01", TOKEN_A, 0L)

        assertNotNull(attempt.reservation)
        assertEquals(CentralConnectAdmission.Outcome.ADMITTED, attempt.outcome)
    }

    // --- priority ---------------------------------------------------------

    @Test
    fun aContactWithMailWinsAContestedSlot() {
        val admission = admissionWith(maxActive = 2, unknownReserve = 1)
        admission.startSession()
        remember(admission, "bob", TOKEN_BOB, BOB)
        occupy(admission, "unknown:1", "tok1", nowMs = 0L)
        occupy(admission, "unknown:2", "tok2", nowMs = 0L)

        val attempt = admission.tryReserve("bob", TOKEN_BOB, HELD)

        assertNotNull(attempt.reservation)
        assertEquals(CentralConnectAdmission.Outcome.PREEMPTED, attempt.outcome)
        assertEquals("unknown:1", attempt.preemptedAddress)
        assertEquals(2, admission.activeCount())
    }

    @Test
    fun mailOutranksAnIdleContactForTheLastSlot() {
        val admission = admissionWith(maxActive = 3, unknownReserve = 1)
        admission.startSession()
        remember(admission, "bob", TOKEN_BOB, BOB)
        occupy(admission, "unknown:1", "tok1", nowMs = 0L)
        occupy(admission, "alice", TOKEN_ALICE, nowMs = 0L, identifyAs = ALICE)
        occupy(admission, "carol", TOKEN_CAROL, nowMs = 0L, identifyAs = CAROL)

        val attempt = admission.tryReserve("bob", TOKEN_BOB, HELD)

        // Not the reserved unknown, and not the other contact-with-mail --
        // the idle contact is the only thing that ranks below the candidate.
        assertEquals("alice", attempt.preemptedAddress)
    }

    @Test
    fun anIdleContactDoesNotDisplaceAContactWithMail() {
        val admission = admissionWith(maxActive = 2, unknownReserve = 0)
        admission.startSession()
        remember(admission, "alice", TOKEN_ALICE, ALICE)
        occupy(admission, "bob", TOKEN_BOB, nowMs = 0L, identifyAs = BOB)
        occupy(admission, "carol", TOKEN_CAROL, nowMs = 0L, identifyAs = CAROL)

        val attempt = admission.tryReserve("alice", TOKEN_ALICE, HELD)

        assertNull(attempt.reservation)
        assertTrue(attempt.atCapacity)
    }

    @Test
    fun aFreshLinkIsNotEvictedBeforeItHasHadItsTurn() {
        val admission = admissionWith(maxActive = 1, unknownReserve = 0)
        admission.startSession()
        remember(admission, "bob", TOKEN_BOB, BOB)
        occupy(admission, "unknown:1", "tok1", nowMs = 0L)

        // Same second: evicting here is how two peers of adjacent rank end up
        // trading one slot on every scan callback.
        assertNull(admission.tryReserve("bob", TOKEN_BOB, 1_000L).reservation)
        assertNotNull(admission.tryReserve("bob", TOKEN_BOB, HELD).reservation)
    }

    // --- anti-starvation --------------------------------------------------

    @Test
    fun unknownsKeepTheirReservedSlotAgainstAContactWithMail() {
        val admission = admissionWith(maxActive = 1, unknownReserve = 1)
        admission.startSession()
        remember(admission, "bob", TOKEN_BOB, BOB)
        occupy(admission, "unknown:1", "tok1", nowMs = 0L)

        val attempt = admission.tryReserve("bob", TOKEN_BOB, HELD)

        assertNull(attempt.reservation)
        assertTrue(attempt.atCapacity)
        assertEquals(1, admission.activeCount())
    }

    @Test
    fun aNewFriendCanStillGetAFirstConnectionThroughAFullBudgetOfContacts() {
        val admission = admissionWith(maxActive = 2, unknownReserve = 1)
        admission.startSession()
        occupy(admission, "alice", TOKEN_ALICE, nowMs = 0L, identifyAs = ALICE)
        occupy(admission, "carol", TOKEN_CAROL, nowMs = 0L, identifyAs = CAROL)

        val attempt = admission.tryReserve("stranger", "tok-new", HELD)

        assertNotNull(attempt.reservation)
        // The idle contact gives way, never the one holding undelivered mail.
        assertEquals("alice", attempt.preemptedAddress)
    }

    @Test
    fun aSecondStrangerDoesNotKeepEvictingContacts() {
        val admission = admissionWith(maxActive = 2, unknownReserve = 1)
        admission.startSession()
        occupy(admission, "alice", TOKEN_ALICE, nowMs = 0L, identifyAs = ALICE)
        occupy(admission, "unknown:1", "tok1", nowMs = 0L)

        // The reserve is already met, so the next stranger waits its turn.
        assertNull(admission.tryReserve("unknown:2", "tok2", HELD).reservation)
    }

    private fun admissionWith(maxActive: Int, unknownReserve: Int) = CentralConnectAdmission(
        maxActive = maxActive,
        selfInstanceToken = SELF,
        unknownReserve = unknownReserve,
        minHoldMs = MIN_HOLD,
        standingOf = { userIdHex ->
            when (userIdHex) {
                BOB, CAROL -> BlePeerStanding.CONTACT_WITH_MAIL
                ALICE -> BlePeerStanding.CONTACT
                else -> BlePeerStanding.UNKNOWN
            }
        },
    )

    /**
     * Teaches the policy which user id an advertised token belongs to, then
     * frees the slot again. Priority is only knowable for a peer this process
     * has already met once -- a contact whose token has never been seen HELLO
     * in is indistinguishable from a stranger before connecting, which is the
     * honest limit of what an advertisement carries.
     */
    private fun remember(
        admission: CentralConnectAdmission,
        address: String,
        token: String,
        userIdHex: String,
    ) {
        assertNotNull(admission.tryReserve(address, token, 0L).reservation)
        admission.onIdentified(address, userIdHex)
        admission.disconnect(address)
    }

    /** Takes a slot and, when named, lets the policy rank it by standing. */
    private fun occupy(
        admission: CentralConnectAdmission,
        address: String,
        token: String?,
        nowMs: Long,
        identifyAs: String? = null,
    ) {
        val reservation = admission.tryReserve(address, token, nowMs).reservation
        assertNotNull("expected $address to take a slot", reservation)
        if (identifyAs != null) assertNull(admission.onIdentified(address, identifyAs))
    }

    private companion object {
        const val SELF = "5e1f5e1f5e1f5e1f"
        const val TOKEN_A = "aaaaaaaaaaaaaaaa"
        const val TOKEN_ALICE = "a11ce00000000000"
        const val TOKEN_BOB = "b0b0000000000000"
        const val TOKEN_CAROL = "ca401a0000000000"
        const val ALICE = "a11ce"
        const val BOB = "b0b"
        const val CAROL = "ca401"
        const val MIN_HOLD = 20_000L

        /** A now-value far enough past 0 that every held slot is evictable. */
        const val HELD = 60_000L
    }
}
