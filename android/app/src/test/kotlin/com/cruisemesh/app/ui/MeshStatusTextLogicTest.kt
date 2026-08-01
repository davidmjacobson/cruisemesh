package com.cruisemesh.app.ui

import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MeshStatusTextLogicTest {

    @Test
    fun `active with nearby peers and healthy Cruise Pass`() {
        val status = MeshStatusTextLogic.build(
            MeshRuntimeState.ACTIVE,
            3,
            RelayHealth.Ok(0L),
            InternetDeliveryService.CRUISE_PASS,
        )
        assertEquals("Mesh on · 3 nearby · Cruise Pass ✓", status.text)
        assertEquals(MeshStatusDotColor.GREEN, status.dot)
    }

    @Test
    fun `active with no peers and healthy Cruise Pass`() {
        val status = MeshStatusTextLogic.build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.Ok(0L),
            InternetDeliveryService.CRUISE_PASS,
        )
        assertEquals("Mesh on · Cruise Pass ✓", status.text)
        assertEquals(MeshStatusDotColor.BLUE, status.dot)
    }

    @Test
    fun `active with nearby peers and no internet`() {
        val status = MeshStatusTextLogic.build(
            MeshRuntimeState.ACTIVE,
            2,
            RelayHealth.NoInternet,
            InternetDeliveryService.CRUISE_PASS,
        )
        assertEquals("Mesh on · 2 nearby · no internet", status.text)
        assertEquals(MeshStatusDotColor.GREEN, status.dot)
    }

    @Test
    fun `active with no peers and no internet is fully offline copy`() {
        val status = MeshStatusTextLogic.build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.NoInternet,
            InternetDeliveryService.CRUISE_PASS,
        )
        assertEquals("Mesh on · offline", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with Cruise Pass failing`() {
        val status = MeshStatusTextLogic.build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.Failing(0L),
            InternetDeliveryService.CRUISE_PASS,
        )
        assertEquals("Mesh on · Cruise Pass unreachable", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with own token rejected names the cause instead of generic unreachable`() {
        val status = MeshStatusTextLogic.build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.TokenRejected(0L),
            InternetDeliveryService.CRUISE_PASS,
        )
        assertEquals("Mesh on · Cruise Pass token rejected", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `never set up internet delivery says nothing about it and does not warn`() {
        val status = MeshStatusTextLogic.build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.NoConfig,
            null,
        )
        assertEquals("Mesh on", status.text)
        assertEquals(MeshStatusDotColor.NEUTRAL, status.dot)
    }

    @Test
    fun `never set up internet delivery still reports nearby peers`() {
        val status = MeshStatusTextLogic.build(
            MeshRuntimeState.ACTIVE,
            2,
            RelayHealth.NoConfig,
            null,
        )
        assertEquals("Mesh on · 2 nearby", status.text)
        assertEquals(MeshStatusDotColor.GREEN, status.dot)
    }

    @Test
    fun `never set up internet delivery is quiet whatever the peer count`() {
        for (nearby in 0..4) {
            val status = MeshStatusTextLogic.build(
                MeshRuntimeState.ACTIVE,
                nearby,
                RelayHealth.NoConfig,
                null,
            )
            assertFalse(status.text.contains("set up", ignoreCase = true))
            assertFalse(status.text.contains("relay", ignoreCase = true))
            assertFalse(status.text.contains("Cruise Pass", ignoreCase = true))
            assertNotEquals(MeshStatusDotColor.AMBER, status.dot)
        }
    }

    // Characterization: this is what the pill does today for a saved-but-unchecked
    // card, pinned here so the quiet-when-never-set-up change above cannot silently
    // swallow it. Whether this copy is right for a card that is merely awaiting its
    // first check -- Settings says "Setup is saved and will be checked when
    // CruiseMesh runs" for the same state -- is a separate question.
    @Test
    fun `a saved pass the mesh has not checked yet keeps its nudge`() {
        for (service in InternetDeliveryService.entries) {
            val status = MeshStatusTextLogic.build(
                MeshRuntimeState.ACTIVE,
                0,
                RelayHealth.NoConfig,
                service,
            )
            assertEquals("Mesh on · no internet delivery set up", status.text)
            assertEquals(MeshStatusDotColor.AMBER, status.dot)
        }
    }

    @Test
    fun `official service status copy uses Cruise Pass branding`() {
        val healthStates = listOf(
            RelayHealth.Ok(0L),
            RelayHealth.Checking,
            RelayHealth.Failing(0L),
            RelayHealth.Expired(0L),
            RelayHealth.Suspended(0L),
            RelayHealth.TokenRejected(0L),
            RelayHealth.QuotaFull(0L),
            RelayHealth.MessageTooLarge(0L),
            RelayHealth.RateLimited(0L),
        )

        for (health in healthStates) {
            val copy = MeshStatusTextLogic.build(
                MeshRuntimeState.ACTIVE,
                0,
                health,
                InternetDeliveryService.CRUISE_PASS,
            ).text
            assertFalse(copy.contains("relay", ignoreCase = true))
        }
    }

    @Test
    fun `custom service status copy uses relay terminology`() {
        for (health in listOf(RelayHealth.Ok(0L), RelayHealth.Checking, RelayHealth.Failing(0L))) {
            val copy = MeshStatusTextLogic.build(
                MeshRuntimeState.ACTIVE,
                0,
                health,
                InternetDeliveryService.CUSTOM_RELAY,
            ).text
            assertTrue(copy.contains("relay", ignoreCase = true))
            assertFalse(copy.contains("Cruise Pass", ignoreCase = true))
        }
    }

    @Test
    fun `non-active runtime states pass their label through unchanged with a neutral dot`() {
        for (state in listOf(MeshRuntimeState.STOPPED, MeshRuntimeState.STARTING, MeshRuntimeState.NO_BLUETOOTH)) {
            val status = MeshStatusTextLogic.build(
                state,
                5,
                RelayHealth.Ok(0L),
                InternetDeliveryService.CRUISE_PASS,
            )
            assertEquals(state.label, status.text)
            assertEquals(MeshStatusDotColor.NEUTRAL, status.dot)
        }
    }
}
