package com.cruisemesh.app.ui

import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth
import org.junit.Assert.assertEquals
import org.junit.Test

class MeshStatusTextLogicTest {

    @Test
    fun `active with nearby peers and healthy relay`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 3, RelayHealth.Ok(0L))
        assertEquals("Mesh on · 3 nearby · relay ✓", status.text)
        assertEquals(MeshStatusDotColor.GREEN, status.dot)
    }

    @Test
    fun `active with no peers and healthy relay`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, RelayHealth.Ok(0L))
        assertEquals("Mesh on · relay ✓", status.text)
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
    fun `active with relay failing`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, RelayHealth.Failing(0L))
        assertEquals("Mesh on · relay unreachable", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with no relay configured`() {
        val status = MeshStatusTextLogic.build(MeshRuntimeState.ACTIVE, 0, RelayHealth.NoConfig)
        assertEquals("Mesh on · no relay set up", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
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
