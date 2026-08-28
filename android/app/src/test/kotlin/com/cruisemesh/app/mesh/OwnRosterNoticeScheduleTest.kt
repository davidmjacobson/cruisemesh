package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.coreOwnRosterNoticeReofferIntervalMs

/**
 * §10 step 5's re-offer schedule -- the half of the removal notice that was
 * missing, and the reason a phone that had been removed from its person's
 * devices went on believing it was linked while sitting on the same Wi-Fi as
 * the phone that removed it.
 */
class OwnRosterNoticeScheduleTest {
    private val capable = OwnRosterNoticePolicy.CAPABILITY_BIT
    private val reofferIntervalMs = coreOwnRosterNoticeReofferIntervalMs()

    @Test
    fun `a link that has just said hello is owed the roster immediately`() {
        val schedule = OwnRosterNoticeSchedule()
        schedule.noteHello2("lan:a", capable)
        assertEquals(capable, schedule.dueCapabilities("lan:a", 1_000))
    }

    @Test
    fun `a link nobody has said hello on is owed nothing`() {
        val schedule = OwnRosterNoticeSchedule()
        assertNull(schedule.dueCapabilities("lan:a", 1_000))
    }

    /**
     * The field case, in one test: the removal happened *after* the HELLO2 that
     * used to be the notice's only trigger. Edge-triggered, the removed phone
     * never hears; level-triggered, it hears on the next re-offer.
     */
    @Test
    fun `a removal after the meeting still reaches the link that is already up`() {
        val schedule = OwnRosterNoticeSchedule()
        val met = 10_000L
        schedule.noteHello2("lan:a", capable)
        // The one offer the shipped build made, at the meeting.
        assertEquals(capable, schedule.dueCapabilities("lan:a", met))
        schedule.noteOffered("lan:a", met)

        // The person removes the other device a few seconds later. Nothing on
        // this link changes: no new HELLO, no new capability exchange.
        assertNull(schedule.dueCapabilities("lan:a", met + 5_000))

        // The re-offer is what carries the news, and it is due on core's
        // cadence rather than on any event this phone has to have seen.
        assertEquals(
            capable,
            schedule.dueCapabilities("lan:a", met + reofferIntervalMs),
        )
    }

    @Test
    fun `a phone that cannot read a notice is never sent one`() {
        val schedule = OwnRosterNoticeSchedule()
        schedule.noteHello2("lan:a", 0u)
        assertNull(schedule.dueCapabilities("lan:a", 1_000))
        assertNull(schedule.dueCapabilities("lan:a", reofferIntervalMs * 10))
    }

    @Test
    fun `a closed link is owed nothing and a new one starts owed again`() {
        val schedule = OwnRosterNoticeSchedule()
        schedule.noteHello2("lan:a", capable)
        schedule.noteOffered("lan:a", 1_000)
        schedule.forget("lan:a")
        assertNull(schedule.dueCapabilities("lan:a", 2_000))

        schedule.noteHello2("lan:b", capable)
        assertEquals(capable, schedule.dueCapabilities("lan:b", 2_000))
        schedule.clear()
        assertNull(schedule.dueCapabilities("lan:b", 3_000))
    }

    @Test
    fun `a second hello on a live link does not reset its cadence`() {
        val schedule = OwnRosterNoticeSchedule()
        schedule.noteHello2("lan:a", capable)
        schedule.noteOffered("lan:a", 1_000)
        // A re-sent HELLO2 (a reconnect racing the reader, a peer repeating
        // itself) must not turn the timer into a per-HELLO spray.
        schedule.noteHello2("lan:a", capable)
        assertNull(schedule.dueCapabilities("lan:a", 1_500))
    }
}
