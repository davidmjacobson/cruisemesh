package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class LanEndpointHintTest {
    @Test
    fun endpointHintRoundTrips() {
        val hint = LanEndpointHint(
            instanceToken = "0123456789abcdef",
            endpoint = LanManualEndpoint("10.154.189.58", 45_892),
        )

        assertEquals(hint, decodeLanEndpointHint(encodeLanEndpointHint(hint)))
    }

    @Test
    fun malformedOrUnsupportedHintsAreRejected() {
        assertNull(decodeLanEndpointHint(byteArrayOf()))
        assertNull(decodeLanEndpointHint(byteArrayOf(LAN_ENDPOINT_HINT_FRAME_TYPE, 2)))

        val valid = encodeLanEndpointHint(
            LanEndpointHint(
                instanceToken = "0123456789abcdef",
                endpoint = LanManualEndpoint("10.0.0.2", 45_892),
            ),
        )
        assertNull(decodeLanEndpointHint(valid.copyOf(valid.size - 1)))
    }
}
