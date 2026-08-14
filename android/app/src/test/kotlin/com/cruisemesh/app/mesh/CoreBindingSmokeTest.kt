package com.cruisemesh.app.mesh

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreConnectionHealthInput
import uniffi.cruisemesh_core.CoreDirectLink
import uniffi.cruisemesh_core.CoreDirectPathState
import uniffi.cruisemesh_core.CoreFamilyRelayBackoff
import uniffi.cruisemesh_core.CoreFamilyRelayPacer
import uniffi.cruisemesh_core.CoreMeshRuntime
import uniffi.cruisemesh_core.CorePersonAttention
import uniffi.cruisemesh_core.CorePersonHealthInput
import uniffi.cruisemesh_core.CorePersonReach
import uniffi.cruisemesh_core.CoreRelayFault
import uniffi.cruisemesh_core.CoreRelayPassHealth
import uniffi.cruisemesh_core.CoreRelayPathState
import uniffi.cruisemesh_core.CoreRelayRerunAction
import uniffi.cruisemesh_core.coreClassifyConnectionHealth
import uniffi.cruisemesh_core.coreFamilyRelayBackoffDelayMs
import uniffi.cruisemesh_core.coreFamilyRelayJitterMs
import uniffi.cruisemesh_core.coreGroupPeople
import uniffi.cruisemesh_core.corePersonAttentionRank
import uniffi.cruisemesh_core.corePersonIsReachableNow
import uniffi.cruisemesh_core.corePersonReach
import uniffi.cruisemesh_core.coreRelayPassHealth
import uniffi.cruisemesh_core.coreRelayRerunAction
import uniffi.cruisemesh_core.voiceCaptureDrag
import uniffi.cruisemesh_core.voiceCapturePlan
import uniffi.cruisemesh_core.voiceCapturePress
import uniffi.cruisemesh_core.voiceCaptureIdleState
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.FriendCard
import uniffi.cruisemesh_core.createSharedFriendCard
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.parseFriendText

/**
 * The *shape* of the UniFFI boundary, not the policy behind it.
 *
 * Generated bindings are checked by a drift gate that regenerates and diffs
 * (`rust.yml`), which proves the checked-in files match the core. It cannot
 * prove the generated marshalling actually carries a value across intact --
 * UniFFI verifies its per-function checksums at *runtime*, and enum
 * discriminants, optional fields, byte arrays, and nested records each have
 * their own lowering path that only executing them exercises.
 *
 * So these assert only what a marshalling bug would break: that every variant
 * of an enum survives a trip through Rust, that an absent optional stays
 * absent and a present one keeps its value, that bytes come back byte-equal,
 * and that a record's fields land in the fields they left from. Every rule
 * about *what the answers mean* is tested in the core module that owns it
 * (`core/src/connection_health.rs`, `core/src/session/relay_policy.rs`) and
 * must not be restated here. `CoreBindingSmokeTests.swift` is the same file
 * for the other shell.
 *
 * The reverse is true too, and matters more: these are not a second drift
 * check. Both shells build their bindings fresh before tests run, so neither
 * suite ever loads a *committed* one. They catch marshalling bugs; only the
 * `rust.yml` diff catches a checked-in binding going stale.
 */
class CoreBindingSmokeTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }

        /** Fixed instant; nothing here depends on which one. */
        const val NOW = 1_760_000_000_000L
    }

    // -- enum discriminants -------------------------------------------------

    /**
     * Every declared variant lowers into Rust and comes back distinguishable.
     * Iterated rather than listed so a variant added later is covered without
     * anyone remembering to add it here. A discriminant that shifted by one
     * lands out of range and panics in Rust; one that collided returns a
     * duplicate rank.
     */
    @Test
    fun `every attention variant crosses the boundary distinctly`() {
        val ranks = CorePersonAttention.entries.map { corePersonAttentionRank(it) }
        assertEquals(CorePersonAttention.entries.size, ranks.toSet().size)
    }

    /**
     * A second enum, lowered through a different signature: every declared
     * constant reaches Rust as a discriminant Rust recognises, so a shifted
     * one lands out of range and panics rather than answering.
     *
     * Deliberately not asserted: *which* variants answer which way. Which
     * reaches count as reachable is policy, owned and pinned by
     * `core/src/connection_health.rs`; restating it here would turn a future
     * policy change into a red marshalling test. Only that the answers are not
     * all identical, which is what a total discriminant collapse would look
     * like from this side.
     */
    @Test
    fun `every reach variant lowers into a discriminant rust recognises`() {
        val answers = CorePersonReach.entries.map { corePersonIsReachableNow(it) }
        assertEquals(CorePersonReach.entries.size, answers.size)
        assertEquals(2, answers.toSet().size)
    }

    // -- optional fields ----------------------------------------------------

    /**
     * Three distinct answers: the absent form is not confused with a present
     * one, and two different present values are not confused with each other
     * — so the payload of `Some(..)` is genuinely carried, not just its
     * presence. Which link maps to which reach is the core's business, so it
     * is the distinctness that is asserted and not the mapping.
     */
    @Test
    fun `an optional argument carries both its absent and present forms`() {
        val absent = corePersonReach(null, 0L, false, NOW)
        val bluetooth = corePersonReach(CoreDirectLink.BLUETOOTH, 0L, false, NOW)
        val localWifi = corePersonReach(CoreDirectLink.LOCAL_WIFI, 0L, false, NOW)
        assertEquals(3, setOf(absent, bluetooth, localWifi).size)
    }

    // -- byte arrays and record round trips ---------------------------------

    /**
     * `userId` is passed in and echoed back untouched, so both directions of
     * the byte-array converter are on the hook. The bytes deliberately include
     * `0x00`, `0x80`, and `0xFF`: a converter that treated them as signed, as
     * text, or as NUL-terminated would corrupt exactly those. The second
     * person also pins an optional *record field* arriving unset, against the
     * first arriving with its value intact.
     */
    @Test
    fun `byte array and optional record fields survive the round trip`() {
        val awkward = byteArrayOf(0x00, 0x7F, 0x80.toByte(), 0xFF.toByte(), 0x01)
        val empty = ByteArray(0)
        val groups = coreGroupPeople(
            listOf(
                person(awkward, "Awkward", attention = CorePersonAttention.SETUP_REJECTED),
                person(empty, "Empty", attention = null),
            ),
            ownRelayUsable = false,
            nowMs = NOW,
        )
        val placements = groups.needsAttention + groups.reachableNow + groups.otherPeople
        assertEquals(2, placements.size)
        val set = placements.single { it.userId.isNotEmpty() }
        val unset = placements.single { it.userId.isEmpty() }
        assertArrayEquals(awkward, set.userId)
        assertEquals(CorePersonAttention.SETUP_REJECTED, set.attention)
        assertArrayEquals(empty, unset.userId)
        assertNull(unset.attention)
    }

    /**
     * A record in, a record with a nested record out. The three counts are
     * copied verbatim by the core, so they pin unsigned marshalling in both
     * directions -- including a value with its top bit set, which a converter
     * that used a signed 32-bit int would deliver as a negative number.
     */
    @Test
    fun `nested record fields land where they left from`() {
        val report = coreClassifyConnectionHealth(
            CoreConnectionHealthInput(
                runtime = CoreMeshRuntime.ACTIVE,
                bluetooth = CoreDirectPathState.AVAILABLE,
                bluetoothLinks = 3u,
                localWifi = CoreDirectPathState.AVAILABLE,
                localWifiLinks = UInt.MAX_VALUE,
                relay = CoreRelayPathState.NOT_SET_UP,
                validatedInternet = true,
                nearbyFriendCount = 7u,
                checkingSinceMs = 0L,
                nowMs = NOW,
            ),
        )
        assertEquals(3u, report.evidence.bluetoothLinks)
        assertEquals(UInt.MAX_VALUE, report.evidence.localWifiLinks)
        assertEquals(7u, report.evidence.nearbyFriendCount)
    }

    // -- relay policy shapes ------------------------------------------------
    //
    // `core/src/session/relay_policy.rs` added shapes none of the tests above
    // reach: two objects that hold state across calls, two enums that are
    // returned rather than passed, an optional enum *argument*, and a bare
    // byte-array argument. The vector suites in FamilyRelayBackpressureTest /
    // RelayRerunPolicyTest / RelayFaultPolicyTest do execute these lowering
    // paths today, but they execute them by reading an exported table -- so if
    // those tables are ever moved behind a feature or trimmed, the coverage
    // leaves with them. These do not read a table, and so they stay.
    //
    // Same rule as everything above: nothing here asserts which answer means
    // what. That is `RATE-01`, pinned in the core.

    /**
     * A `uniffi::Object` that holds state on the Rust side: the handle must
     * survive between calls, or the second reservation would answer as if it
     * were the first. Says nothing about the interval, only that the two calls
     * reached the same instance.
     */
    @Test
    fun `an object handle carries rust-side state between calls`() {
        CoreFamilyRelayPacer().use { pacer ->
            val first = pacer.reserve(0L)
            val second = pacer.reserve(0L)
            assertNotEquals(first, second)
        }
    }

    /**
     * The other object, plus the unsigned counter it returns: a fresh instance
     * starts at zero, one call moves it, and the reset call moves it back --
     * so `UInt` crosses in the returning direction and the handle is genuinely
     * per-instance rather than shared.
     */
    @Test
    fun `object state advances and resets through the boundary`() {
        CoreFamilyRelayBackoff().use { backoff ->
            assertEquals(0u, backoff.consecutiveRateLimits())
            backoff.onRateLimited(0uL, ByteArray(0))
            assertEquals(1u, backoff.consecutiveRateLimits())
            CoreFamilyRelayBackoff().use { other ->
                assertEquals(0u, other.consecutiveRateLimits())
            }
            backoff.onSuccessfulPass()
            assertEquals(0u, backoff.consecutiveRateLimits())
        }
    }

    /**
     * An *optional enum argument* -- a lowering path no other smoke here
     * covers -- and an enum returned by value. The absent form must not be
     * confused with a present one, and a discriminant that shifted would land
     * out of range in Rust and panic rather than answer.
     */
    @Test
    fun `an optional enum argument lowers in both its forms`() {
        val answers = listOf(
            coreRelayPassHealth(null, ownRelaySucceeded = false, anyRelaySucceeded = false),
            coreRelayPassHealth(CoreRelayFault.MAILBOX_FULL, ownRelaySucceeded = false, anyRelaySucceeded = false),
            coreRelayPassHealth(CoreRelayFault.TOKEN_REJECTED, ownRelaySucceeded = false, anyRelaySucceeded = false),
        )
        assertEquals(3, answers.size)
        assertEquals(3, answers.toSet().size)
    }

    /**
     * Every declared variant of the returned enum is reachable, so none of the
     * eight discriminants lifts onto another's name. Which input produces
     * which is the core's business and is not asserted; only that the set is
     * covered.
     */
    @Test
    fun `every pass-health variant lifts back distinctly`() {
        val produced = buildSet {
            for (fault in listOf(null) + CoreRelayFault.entries) {
                for (own in listOf(false, true)) {
                    for (any in listOf(false, true)) {
                        add(coreRelayPassHealth(fault, own, any))
                    }
                }
            }
        }
        assertEquals(CoreRelayPassHealth.entries.toSet(), produced)
    }

    /** The third enum, lifted through a different signature. */
    @Test
    fun `every rerun action lifts back distinctly`() {
        val produced = buildSet {
            for (pending in listOf(false, true)) {
                for (canSync in listOf(false, true)) {
                    for (remaining in listOf(-1L, 0L, 30_000L)) {
                        add(coreRelayRerunAction(pending, canSync, remaining))
                    }
                }
            }
        }
        assertEquals(CoreRelayRerunAction.entries.toSet(), produced)
    }

    /**
     * A bare byte-array argument, with the bytes a broken converter corrupts:
     * `0x00`, `0x80`, `0xFF`. Reversing them must change the answer, which a
     * buffer that arrived truncated, NUL-terminated, or empty could not do;
     * and an empty array must be carried as empty rather than as garbage.
     */
    @Test
    fun `a bare byte array argument crosses intact`() {
        val awkward = byteArrayOf(0x00, 0x7F, 0x80.toByte(), 0xFF.toByte(), 0x01)
        val reversed = awkward.reversedArray()
        assertNotEquals(coreFamilyRelayJitterMs(awkward), coreFamilyRelayJitterMs(reversed))
        assertEquals(coreFamilyRelayJitterMs(awkward), coreFamilyRelayJitterMs(awkward.copyOf()))
        // Empty is a value, not a missing argument.
        assertEquals(coreFamilyRelayJitterMs(ByteArray(0)), coreFamilyRelayJitterMs(ByteArray(0)))
    }

    /**
     * `ULong` in both directions, at a value a signed 64-bit converter would
     * deliver as -1. Asserts only that the top bit survived the trip, not what
     * the arithmetic did with it.
     */
    @Test
    fun `an unsigned 64-bit value keeps its top bit across the boundary`() {
        val answer = coreFamilyRelayBackoffDelayMs(ULong.MAX_VALUE, 1u, 0uL)
        assertTrue("top bit lost: $answer", answer > Long.MAX_VALUE.toULong())
    }

    /**
     * `Float` in both directions, and a record nested inside a record beside an
     * enum. The voice-capture plan is the first exported surface here to carry
     * a float at all, so this executes the lowering rather than trusting it.
     */
    @Test
    fun `receipt content optional group id crosses present and absent`() {
        val pairwise = uniffi.cruisemesh_core.ReceiptContent(
            chatId = byteArrayOf(1, 2, 3),
            senderUserId = byteArrayOf(4, 5, 6),
            lamport = 7uL,
            receiptType = 1u,
            groupId = null,
        )
        val encoded = uniffi.cruisemesh_core.encodeReceiptContent(pairwise)
        val decoded = uniffi.cruisemesh_core.decodeReceiptContent(encoded)
        assertNull(decoded.groupId)
        assertEquals(7uL, decoded.lamport)

        val groupId = ByteArray(16) { 0x11 }
        val grouped = pairwise.copy(groupId = groupId)
        val groupDecoded = uniffi.cruisemesh_core.decodeReceiptContent(
            uniffi.cruisemesh_core.encodeReceiptContent(grouped),
        )
        assertArrayEquals(groupId, groupDecoded.groupId)
    }

    @Test
    fun `a float crosses the boundary and lands in a nested record`() {
        val plan = voiceCapturePlan()
        assertTrue("cancel threshold arrived as ${plan.cancelSlideDp}", plan.cancelSlideDp > 1f)

        val holding = voiceCapturePress(voiceCaptureIdleState()).state
        assertTrue(voiceCaptureDrag(holding, -(plan.cancelSlideDp + 1f), 0f).state.cancelArmed)
        assertTrue(!voiceCaptureDrag(holding, -(plan.cancelSlideDp - 1f), 0f).state.cancelArmed)
    }

    /**
     * FriendCard gained an optional roster-head field (WPT / CMFRIEND4).
     * Absent and present must stay distinguishable after a nested-record
     * round trip; a converter that dropped the new field would collapse them.
     */
    @Test
    fun `friend card optional roster head hash survives a nested record trip`() {
        val owner = generateIdentity()
        val absent = FriendCard(
            name = "Dana",
            signPk = owner.signPk,
            agreePk = owner.agreePk,
            relayUrl = null,
            relayToken = null,
            signature = null,
            rosterHeadHash = null,
        )
        val hash = ByteArray(32) { 0xAB.toByte() }
        val present = absent.copy(rosterHeadHash = hash)
        val echoedAbsent = createSharedFriendCard(owner, absent, 1u, NOW).card
        val echoedPresent = createSharedFriendCard(owner, present, 1u, NOW).card
        assertNull(echoedAbsent.rosterHeadHash)
        assertArrayEquals(hash, echoedPresent.rosterHeadHash)
    }

    /**
     * The new UnsupportedLink error variant must lift as itself. A shifted
     * discriminant would land on another CoreException case or panic.
     */
    @Test
    fun `unsupported link error variant lifts`() {
        try {
            parseFriendText("CMFRIEND5:abc")
            throw AssertionError("expected UnsupportedLink")
        } catch (_: CoreException.UnsupportedLink) {
            // shape only
        }
    }

    private fun person(
        userId: ByteArray,
        displayName: String,
        attention: CorePersonAttention?,
    ) = CorePersonHealthInput(
        userId = userId,
        displayName = displayName,
        blocked = false,
        directLink = null,
        presenceLastSeenMs = 0L,
        lastSeenMs = 0L,
        attention = attention,
        attentionSinceMs = 0L,
    )
}
