package com.cruisemesh.app.devicelink

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * §9.4 says "may not advertise ANYTHING". This pins the half of that sentence
 * the mesh service owns, and it exists because of a real gap: the disallow
 * branch used to stop BLE alone, leaving the LAN transport publishing this
 * phone over NSD, accepting connections and answering handshakes for the whole
 * pre-activation window. "Invisible on the mesh" while visible on the loudest
 * transport in the house is not invisible.
 *
 * The branch itself lives inside a foreground service with a BLE stack and a
 * socket accept loop in it and cannot be unit-tested; which radios it moves can
 * be, and this is that seam.
 */
class LinkRadioPlanTest {

    @Test
    fun goingSilentTakesLanDownAndNotJustBle() {
        val steps = LinkRadioPlan.stepsFor(allowed = false)
        assertTrue(
            "a phone that must be invisible has to stop advertising over LAN too",
            steps.contains(LinkRadioStep.STOP_LAN),
        )
        assertTrue(steps.contains(LinkRadioStep.STOP_BLE))
        assertTrue(
            "nothing is brought up on the way down",
            steps.none { it == LinkRadioStep.START_LAN || it == LinkRadioStep.START_BLE },
        )
        // Stop shouting before you stop listening: the BLE roles are still
        // reacting to link state that tearing LAN down would change under them.
        assertEquals(listOf(LinkRadioStep.STOP_BLE, LinkRadioStep.STOP_LAN), steps)
    }

    @Test
    fun becomingVisibleBringsBackEverythingItTookAway() {
        val down = LinkRadioPlan.stepsFor(allowed = false)
        val up = LinkRadioPlan.stepsFor(allowed = true)

        assertEquals(listOf(LinkRadioStep.START_LAN, LinkRadioStep.START_BLE), up)
        // Every transport the window silenced comes back. A window that could
        // take a radio down and not return it would strand a phone that had
        // just been successfully adopted.
        val stopped = down.map {
            when (it) {
                LinkRadioStep.STOP_BLE -> LinkRadioStep.START_BLE
                LinkRadioStep.STOP_LAN -> LinkRadioStep.START_LAN
                else -> it
            }
        }.toSet()
        assertEquals(stopped, up.toSet())
    }
}
