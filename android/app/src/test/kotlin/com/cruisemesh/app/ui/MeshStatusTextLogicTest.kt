package com.cruisemesh.app.ui

import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class MeshStatusTextLogicTest {

    @Test
    fun `active with nearby peers and healthy Cruise Pass`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 3, RelayHealth.Ok(0L))
        assertEquals("Mesh on · 3 nearby · Cruise Pass ✓", status.text)
        assertEquals(MeshStatusDotColor.GREEN, status.dot)
    }

    @Test
    fun `active with no peers and healthy Cruise Pass`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, RelayHealth.Ok(0L))
        assertEquals("Mesh on · Cruise Pass ✓", status.text)
        assertEquals(MeshStatusDotColor.BLUE, status.dot)
    }

    @Test
    fun `active with nearby peers and no internet`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 2, RelayHealth.NoInternet)
        assertEquals("Mesh on · 2 nearby · no internet", status.text)
        assertEquals(MeshStatusDotColor.GREEN, status.dot)
    }

    @Test
    fun `active with no peers and no internet is fully offline copy`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, RelayHealth.NoInternet)
        assertEquals("Mesh on · offline", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with Cruise Pass failing`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, RelayHealth.Failing(0L))
        assertEquals("Mesh on · Cruise Pass unreachable", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with own token rejected names the cause instead of generic unreachable`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, RelayHealth.TokenRejected(0L))
        assertEquals("Mesh on · Cruise Pass token rejected", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with no Cruise Pass configured`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, RelayHealth.NoConfig)
        assertEquals("Mesh on · no Cruise Pass set up", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active status copy never exposes relay terminology`() {
        val healthStates = listOf(
            RelayHealth.Ok(0L),
            RelayHealth.Checking,
            RelayHealth.NoInternet,
            RelayHealth.NoConfig,
            RelayHealth.Failing(0L),
            RelayHealth.Expired(0L),
            RelayHealth.Suspended(0L),
            RelayHealth.TokenRejected(0L),
            RelayHealth.QuotaFull(0L),
            RelayHealth.MessageTooLarge(0L),
            RelayHealth.RateLimited(0L),
        )

        for (health in healthStates) {
            val copy = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, health).text
            assertFalse(copy.contains("relay", ignoreCase = true))
        }
    }

    @Test
    fun `non-active runtime states pass their label through unchanged with a neutral dot`() {
        for (state in listOf(MeshRuntimeState.STOPPED, MeshRuntimeState.STARTING, MeshRuntimeState.NO_BLUETOOTH)) {
            val status = MeshStatusTextLogic.build(state, 5, RelayHealth.Ok(0L))
            assertEquals(state.label, status.text)
            assertEquals(MeshStatusDotColor.NEUTRAL, status.dot)
        }
    }
}
