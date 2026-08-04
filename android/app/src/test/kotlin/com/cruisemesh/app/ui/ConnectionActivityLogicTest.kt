package com.cruisemesh.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.PeerConnectionEventKind
import uniffi.cruisemesh_core.PeerConnectionSummary
import uniffi.cruisemesh_core.PeerConnectionTransport
import uniffi.cruisemesh_core.corePeerTransportIsObserved

class ConnectionActivityLogicTest {

    private fun summary(
        transport: PeerConnectionTransport,
        connected: Long? = null,
        disconnected: Long? = null,
        seen: Long? = null,
        delivered: Long? = null,
        received: Long? = null,
    ) = PeerConnectionSummary(
        userId = byteArrayOf(1, 2, 3),
        transport = transport,
        lastConnectedAtMs = connected,
        lastDisconnectedAtMs = disconnected,
        lastSeenAtMs = seen,
        lastDeliveredAtMs = delivered,
        lastReceivedAtMs = received,
    )

    @Test
    fun `no rows means no status line`() {
        assertNull(latestPeerStatus(emptyList()))
    }

    @Test
    fun `a row with no timestamps at all means no status line`() {
        assertNull(latestPeerStatus(listOf(summary(PeerConnectionTransport.BLUETOOTH))))
    }

    /**
     * The two message directions are separate fields and must map to separate
     * evidence. Reporting an outbound delivery confirmation as an inbound
     * arrival is the defect this whole change exists to remove.
     */
    @Test
    fun `outbound delivery and inbound arrival are distinct evidence`() {
        val delivered = latestPeerStatus(
            listOf(summary(PeerConnectionTransport.SHORE_PASS, delivered = 500L)),
        )
        assertEquals(PeerEvidence.MESSAGE_DELIVERED, delivered?.evidence)

        val received = latestPeerStatus(
            listOf(summary(PeerConnectionTransport.SHORE_PASS, received = 500L)),
        )
        assertEquals(PeerEvidence.MESSAGE_RECEIVED, received?.evidence)
    }

    @Test
    fun `the newest timestamp on a row wins, whichever field it is on`() {
        val status = latestPeerStatus(
            listOf(
                summary(
                    PeerConnectionTransport.BLUETOOTH,
                    connected = 100L,
                    disconnected = 200L,
                    seen = 900L,
                    delivered = 300L,
                    received = 400L,
                ),
            ),
        )
        assertEquals(PeerEvidence.PRESENCE_SEEN, status?.evidence)
        assertEquals(900L, status?.atMs)
    }

    /**
     * Regression guard for the old row-first selection: it picked the row with
     * the newest anything, then re-derived the evidence from a fixed field
     * order, so a stale delivery confirmation could be reported as the latest
     * news while a fresher arrival on another path was dropped.
     */
    @Test
    fun `the newest moment wins across paths, not the first field on a row`() {
        val status = latestPeerStatus(
            listOf(
                summary(PeerConnectionTransport.SHORE_PASS, delivered = 1_000L, seen = 2_000L),
                summary(PeerConnectionTransport.BLUETOOTH, received = 5_000L),
            ),
        )
        assertEquals(PeerEvidence.MESSAGE_RECEIVED, status?.evidence)
        assertEquals(PeerConnectionTransport.BLUETOOTH, status?.transport)
        assertEquals(5_000L, status?.atMs)
    }

    @Test
    fun `the reported path is the one the winning moment happened on`() {
        val status = latestPeerStatus(
            listOf(
                summary(PeerConnectionTransport.BLUETOOTH, seen = 9_000L),
                summary(PeerConnectionTransport.LOCAL_WIFI, received = 1_000L),
            ),
        )
        assertEquals(PeerConnectionTransport.BLUETOOTH, status?.transport)
        assertEquals(PeerEvidence.PRESENCE_SEEN, status?.evidence)
    }

    /** A tie is broken towards the more informative evidence. */
    @Test
    fun `an inbound arrival outranks a link event at the same instant`() {
        val status = latestPeerStatus(
            listOf(summary(PeerConnectionTransport.BLUETOOTH, connected = 700L, received = 700L)),
        )
        assertEquals(PeerEvidence.MESSAGE_RECEIVED, status?.evidence)
    }

    @Test
    fun `every event kind maps to its own evidence`() {
        assertEquals(PeerEvidence.CONNECTED, peerEvidenceOf(PeerConnectionEventKind.CONNECTED))
        assertEquals(PeerEvidence.DISCONNECTED, peerEvidenceOf(PeerConnectionEventKind.DISCONNECTED))
        assertEquals(PeerEvidence.PRESENCE_SEEN, peerEvidenceOf(PeerConnectionEventKind.PRESENCE_SEEN))
        assertEquals(
            PeerEvidence.MESSAGE_DELIVERED,
            peerEvidenceOf(PeerConnectionEventKind.MESSAGE_DELIVERED),
        )
        assertEquals(
            PeerEvidence.MESSAGE_RECEIVED,
            peerEvidenceOf(PeerConnectionEventKind.MESSAGE_RECEIVED),
        )
    }

    @Test
    fun `a path is named exactly when core says it was observed`() {
        // The screen decides between "… via Bluetooth" and the wordless
        // variant by whether transportLabelId returns a string. That decision
        // must agree with core, or one surface starts naming a path the other
        // says was never seen. Every transport is checked, so a variant added
        // later cannot silently pick a default.
        for (transport in PeerConnectionTransport.entries) {
            assertEquals(
                "transportLabelId disagrees with core for $transport",
                corePeerTransportIsObserved(transport),
                transportLabelId(transport) != null,
            )
        }
    }

    @Test
    fun `a carried message names no path`() {
        assertNull(
            "a message another device carried must not be labelled with a radio",
            transportLabelId(PeerConnectionTransport.CARRIED),
        )
    }
}
