package com.cruisemesh.app.ui

import com.cruisemesh.app.R
import com.cruisemesh.app.chat.TickStatus
import org.junit.Assert.assertEquals
import org.junit.Test

class SignalTickTest {

    @Test
    fun `content descriptions use localized state-specific resources`() {
        assertEquals(R.string.ui_sent, tickContentDescriptionResource(TickStatus.SENT))
        assertEquals(R.string.ui_delivered, tickContentDescriptionResource(TickStatus.DELIVERED))
        assertEquals(R.string.ui_read, tickContentDescriptionResource(TickStatus.READ))
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
