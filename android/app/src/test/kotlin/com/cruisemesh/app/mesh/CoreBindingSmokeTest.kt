package com.cruisemesh.app.mesh

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.CoreConnectionHealthInput
import uniffi.cruisemesh_core.CoreDirectLink
import uniffi.cruisemesh_core.CoreDirectPathState
import uniffi.cruisemesh_core.CoreMeshRuntime
import uniffi.cruisemesh_core.CorePersonAttention
import uniffi.cruisemesh_core.CorePersonHealthInput
import uniffi.cruisemesh_core.CorePersonReach
import uniffi.cruisemesh_core.CoreRelayPathState
import uniffi.cruisemesh_core.coreClassifyConnectionHealth
import uniffi.cruisemesh_core.coreGroupPeople
import uniffi.cruisemesh_core.corePersonAttentionRank
import uniffi.cruisemesh_core.corePersonIsReachableNow
import uniffi.cruisemesh_core.corePersonReach

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
 * about *what the answers mean* is tested in `core/src/connection_health.rs`
 * and must not be restated here. `CoreBindingSmokeTests.swift` is the same
 * file for the other shell.
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
