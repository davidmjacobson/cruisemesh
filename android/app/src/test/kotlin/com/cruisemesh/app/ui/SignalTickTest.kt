package com.cruisemesh.app.ui

import com.cruisemesh.app.chat.TickStatus
import org.junit.Assert.assertEquals
import org.junit.Test

class SignalTickTest {

    @Test
    fun `content descriptions are short and state specific`() {
        assertEquals("Sent", tickContentDescription(TickStatus.SENT))
        assertEquals("Delivered", tickContentDescription(TickStatus.DELIVERED))
        assertEquals("Read", tickContentDescription(TickStatus.READ))
    }

    @Test
    fun `legend copy matches the rendered tick state`() {
        assertEquals("Sent: queued for delivery.", tickLegendText(TickStatus.SENT))
        assertEquals(
            "Delivered: received by the contact's device.",
            tickLegendText(TickStatus.DELIVERED),
        )
        assertEquals("Read: viewed by the contact.", tickLegendText(TickStatus.READ))
    }
}
