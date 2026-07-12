package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test

class ContactReachabilityTest {

    @Test
    fun `direct link always wins regardless of every other signal`() {
        val level = ContactReachability.compute(
            directLink = true,
            presenceLastSeenMs = null,
            selfRelayHealthy = false,
            peerLastSeenMs = null,
            nearbyPeerCount = 0,
            nowMs = 1_000L,
        )
        assertEquals(ReachabilityLevel.NEARBY, level)
    }

    @Test
    fun `presence exactly at the online window boundary still counts as online`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = 0L,
            selfRelayHealthy = true,
            peerLastSeenMs = null,
            nearbyPeerCount = 0,
            nowMs = ContactReachability.PRESENCE_ONLINE_WINDOW_MS,
        )
        assertEquals(ReachabilityLevel.ONLINE_RELAY, level)
    }

    @Test
    fun `presence one ms past the online window falls through to the next tier`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = 0L,
            selfRelayHealthy = true,
            peerLastSeenMs = 0L,
            nearbyPeerCount = 0,
            nowMs = ContactReachability.PRESENCE_ONLINE_WINDOW_MS + 1,
        )
        assertEquals(ReachabilityLevel.RECENT, level)
    }

    @Test
    fun `unhealthy relay suppresses ONLINE_RELAY even with fresh presence`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = 0L,
            selfRelayHealthy = false,
            peerLastSeenMs = null,
            nearbyPeerCount = 0,
            nowMs = 0L,
        )
        assertEquals(ReachabilityLevel.OFFLINE, level)
    }

    @Test
    fun `stale own relay health suppresses ONLINE_RELAY even with fresh presence`() {
        val now = 2 * ContactReachability.RELAY_POLL_INTERVAL_MS + 1
        val relayHealthy = ContactReachability.selfRelayHealthy(RelayHealth.Ok(0L), now)
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = now,
            selfRelayHealthy = relayHealthy,
            peerLastSeenMs = null,
            nearbyPeerCount = 0,
            nowMs = now,
        )
        assertEquals(false, relayHealthy)
        assertEquals(ReachabilityLevel.OFFLINE, level)
    }

    @Test
    fun `fresh own relay health includes the two-poll boundary`() {
        assertEquals(
            true,
            ContactReachability.selfRelayHealthy(
                RelayHealth.Ok(0L),
                2 * ContactReachability.RELAY_POLL_INTERVAL_MS,
            ),
        )
    }

    @Test
    fun `unhealthy relay does not suppress RECENT`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = 0L,
            selfRelayHealthy = false,
            peerLastSeenMs = 0L,
            nearbyPeerCount = 0,
            nowMs = ContactReachability.RECENT_WINDOW_MS,
        )
        assertEquals(ReachabilityLevel.RECENT, level)
    }

    @Test
    fun `peerLastSeen exactly at the recent window boundary still counts as recent`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = null,
            selfRelayHealthy = false,
            peerLastSeenMs = 0L,
            nearbyPeerCount = 0,
            nowMs = ContactReachability.RECENT_WINDOW_MS,
        )
        assertEquals(ReachabilityLevel.RECENT, level)
    }

    @Test
    fun `peerLastSeen one ms past the recent window falls through`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = null,
            selfRelayHealthy = false,
            peerLastSeenMs = 0L,
            nearbyPeerCount = 1,
            nowMs = ContactReachability.RECENT_WINDOW_MS + 1,
        )
        assertEquals(ReachabilityLevel.MESH_CARRY, level)
    }

    @Test
    fun `no last-seen evidence but a nearby peer still means MESH_CARRY`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = null,
            selfRelayHealthy = false,
            peerLastSeenMs = null,
            nearbyPeerCount = 1,
            nowMs = 0L,
        )
        assertEquals(ReachabilityLevel.MESH_CARRY, level)
    }

    @Test
    fun `disabled mesh carry collapses nearby peer fallback to OFFLINE`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = null,
            selfRelayHealthy = false,
            peerLastSeenMs = null,
            nearbyPeerCount = 1,
            nowMs = 0L,
            meshCarryEnabled = false,
        )
        assertEquals(ReachabilityLevel.OFFLINE, level)
    }

    @Test
    fun `nothing at all is OFFLINE`() {
        val level = ContactReachability.compute(
            directLink = false,
            presenceLastSeenMs = null,
            selfRelayHealthy = false,
            peerLastSeenMs = null,
            nearbyPeerCount = 0,
            nowMs = 0L,
        )
        assertEquals(ReachabilityLevel.OFFLINE, level)
    }

    @Test
    fun `chatHeaderCopy rounds RECENT to whole minutes and switches to hours past 60m`() {
        assertEquals(
            "Active 5m ago",
            ContactReachability.chatHeaderCopy(ReachabilityLevel.RECENT, 0L, 5 * 60_000L),
        )
        assertEquals(
            "Active 2h ago",
            ContactReachability.chatHeaderCopy(ReachabilityLevel.RECENT, 0L, 120 * 60_000L),
        )
    }

    @Test
    fun `contactDetailsCopy includes relay path detail when presence is known`() {
        assertEquals(
            "Online via relay · Last seen via relay 2m ago",
            ContactReachability.contactDetailsCopy(
                ReachabilityLevel.ONLINE_RELAY,
                peerLastSeenMs = 60_000L,
                presenceLastSeenMs = 60_000L,
                nowMs = 180_000L,
            ),
        )
    }

    @Test
    fun `contactDetailsCopy includes generic last seen detail without relay presence`() {
        assertEquals(
            "Active 1h ago · Last seen 1h ago",
            ContactReachability.contactDetailsCopy(
                ReachabilityLevel.RECENT,
                peerLastSeenMs = 0L,
                presenceLastSeenMs = null,
                nowMs = 60 * 60_000L,
            ),
        )
    }

    @Test
    fun `contentDescriptionSuffix is null for OFFLINE and non-null for every other level`() {
        assertEquals(null, ContactReachability.contentDescriptionSuffix(ReachabilityLevel.OFFLINE))
        for (level in ReachabilityLevel.entries.filter { it != ReachabilityLevel.OFFLINE }) {
            assert(ContactReachability.contentDescriptionSuffix(level) != null) { "expected non-null suffix for $level" }
        }
    }
}
