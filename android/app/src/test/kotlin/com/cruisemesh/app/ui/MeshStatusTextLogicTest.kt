package com.cruisemesh.app.ui

import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.RelayHealth
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreConnectionHealth
import uniffi.cruisemesh_core.CoreConnectionHealthInput
import uniffi.cruisemesh_core.CoreDirectPathState
import uniffi.cruisemesh_core.CoreMeshRuntime
import uniffi.cruisemesh_core.CoreRelayPathState
import uniffi.cruisemesh_core.coreClassifyConnectionHealth

class MeshStatusTextLogicTest {

    @Test
    fun `mesh status dot stays steady when system animations are disabled`() {
        assertFalse(meshStatusDotShouldAnimate(statusWantsPulse = true, systemAnimationsEnabled = false))
    }

    @Test
    fun `mesh status dot animates only for a pulsing state`() {
        assertTrue(meshStatusDotShouldAnimate(statusWantsPulse = true, systemAnimationsEnabled = true))
        assertFalse(meshStatusDotShouldAnimate(statusWantsPulse = false, systemAnimationsEnabled = true))
    }

    private companion object {
        const val NOW = 1_760_000_000_000L

        /**
         * No check outstanding.
         *
         * Zero means "nothing pending" to the core, which treats the bound as
         * already expired -- the settled state a phone spends nearly all its
         * time in. Tests that care about the moments *inside* a check pass a
         * real mark instead.
         */
        const val NO_CHECK = 0L
    }

    @Suppress("LongParameterList")
    private fun build(
        runtimeState: MeshRuntimeState,
        nearbyCount: Int,
        relayHealth: RelayHealth,
        internetDeliveryService: InternetDeliveryService?,
        lanListening: Boolean = true,
        checkingSinceMs: Long = NO_CHECK,
    ) = MeshStatusTextLogic.build(
        runtimeState = runtimeState,
        nearbyCount = nearbyCount,
        relayHealth = relayHealth,
        internetDeliveryService = internetDeliveryService,
        lanListening = lanListening,
        checkingSinceMs = checkingSinceMs,
        nowMs = NOW,
    )

    @Test
    fun `active with nearby peers and healthy Shore Pass`() {
        val status = build(
            MeshRuntimeState.ACTIVE,
            3,
            RelayHealth.Ok(0L),
            InternetDeliveryService.SHORE_PASS,
        )
        assertEquals("Mesh on · 3 nearby · Shore Pass ✓", status.text)
        assertEquals(MeshStatusDotColor.GREEN, status.dot)
        assertEquals(CoreConnectionHealth.READY, status.health)
    }

    @Test
    fun `active with no peers and healthy Shore Pass`() {
        val status = build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.Ok(0L),
            InternetDeliveryService.SHORE_PASS,
        )
        assertEquals("Mesh on · Shore Pass ✓", status.text)
        assertEquals(MeshStatusDotColor.BLUE, status.dot)
        assertEquals(CoreConnectionHealth.READY, status.health)
    }

    @Test
    fun `a friend in the room no longer paints over a degraded phone`() {
        // The divergence this wiring closes. A nearby friend used to force the
        // dot green whatever else was wrong, so the pill read healthy while the
        // Connection details card read "Working, with limits" about the same
        // phone at the same moment.
        val status = build(
            MeshRuntimeState.ACTIVE,
            2,
            RelayHealth.NoInternet,
            InternetDeliveryService.SHORE_PASS,
        )
        assertEquals("Mesh on · 2 nearby · no internet", status.text)
        assertEquals(CoreConnectionHealth.LIMITED, status.health)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with no peers and no internet is fully offline copy`() {
        val status = build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.NoInternet,
            InternetDeliveryService.SHORE_PASS,
        )
        assertEquals("Mesh on · offline", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with Shore Pass failing`() {
        val status = build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.Failing(0L),
            InternetDeliveryService.SHORE_PASS,
        )
        assertEquals("Mesh on · Shore Pass unreachable", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `active with own token rejected names the cause instead of generic unreachable`() {
        val status = build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.TokenRejected(0L),
            InternetDeliveryService.SHORE_PASS,
        )
        assertEquals("Mesh on · Shore Pass token rejected", status.text)
        assertEquals(MeshStatusDotColor.AMBER, status.dot)
    }

    @Test
    fun `never set up internet delivery says nothing about it and does not warn`() {
        val status = build(MeshRuntimeState.ACTIVE, 0, RelayHealth.NoConfig, null)
        assertEquals("Mesh on", status.text)
        assertEquals(MeshStatusDotColor.NEUTRAL, status.dot)
        // No pass is the free default, not a degradation.
        assertEquals(CoreConnectionHealth.READY, status.health)
    }

    @Test
    fun `never set up internet delivery still reports nearby peers`() {
        val status = build(MeshRuntimeState.ACTIVE, 2, RelayHealth.NoConfig, null)
        assertEquals("Mesh on · 2 nearby", status.text)
        assertEquals(MeshStatusDotColor.GREEN, status.dot)
    }

    @Test
    fun `never set up internet delivery is quiet whatever the peer count`() {
        for (nearby in 0..4) {
            val status = build(MeshRuntimeState.ACTIVE, nearby, RelayHealth.NoConfig, null)
            assertFalse(status.text.contains("set up", ignoreCase = true))
            assertFalse(status.text.contains("relay", ignoreCase = true))
            assertFalse(status.text.contains("Shore Pass", ignoreCase = true))
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
            val status = build(MeshRuntimeState.ACTIVE, 0, RelayHealth.NoConfig, service)
            assertEquals("Mesh on · no internet delivery set up", status.text)
            assertEquals(MeshStatusDotColor.AMBER, status.dot)
        }
    }

    @Test
    fun `a check still running shows no color rather than a premature warning`() {
        // The bound the core owns, observed from the pill: while the first
        // check on a saved pass is outstanding, the dot makes no claim. It
        // resolves to the honest state once the window is up, which the test
        // above pins.
        val status = build(
            MeshRuntimeState.ACTIVE,
            0,
            RelayHealth.NoConfig,
            InternetDeliveryService.SHORE_PASS,
            checkingSinceMs = NOW - 1_000L,
        )
        assertEquals(CoreConnectionHealth.READY, status.health)
        assertEquals(MeshStatusDotColor.NEUTRAL, status.dot)
    }

    @Test
    fun `official service status copy uses Shore Pass branding`() {
        val healthStates = listOf(
            RelayHealth.Ok(0L),
            RelayHealth.Checking,
            RelayHealth.Failing(0L),
            RelayHealth.Expired(0L),
            RelayHealth.ExpiredReadOnly(0L),
            RelayHealth.Suspended(0L),
            RelayHealth.TokenRejected(0L),
            RelayHealth.QuotaFull(0L),
            RelayHealth.MessageTooLarge(0L),
            RelayHealth.RateLimited(0L),
        )

        for (health in healthStates) {
            val copy = build(
                MeshRuntimeState.ACTIVE,
                0,
                health,
                InternetDeliveryService.SHORE_PASS,
            ).text
            assertFalse(copy.contains("relay", ignoreCase = true))
        }
    }

    @Test
    fun `custom service status copy uses relay terminology`() {
        for (health in listOf(RelayHealth.Ok(0L), RelayHealth.Checking, RelayHealth.Failing(0L))) {
            val copy = build(
                MeshRuntimeState.ACTIVE,
                0,
                health,
                InternetDeliveryService.CUSTOM_RELAY,
            ).text
            assertTrue(copy.contains("relay", ignoreCase = true))
            assertFalse(copy.contains("Shore Pass", ignoreCase = true))
        }
    }

    @Test
    fun `non-active runtime states pass their label through unchanged`() {
        for (state in listOf(
            MeshRuntimeState.STOPPED,
            MeshRuntimeState.STARTING,
            MeshRuntimeState.NO_BLUETOOTH,
        )) {
            val status = build(state, 5, RelayHealth.Ok(0L), InternetDeliveryService.SHORE_PASS)
            assertEquals(state.label, status.text)
        }
    }

    @Test
    fun `a stopped mesh and a dark radio are warnings, not quiet neutral dots`() {
        // Both used to be neutral no matter what, which is how a phone that had
        // stopped participating altogether could look the same as one waiting
        // quietly for a friend. The card calls both of these out; the pill now
        // agrees.
        val stopped = build(
            MeshRuntimeState.STOPPED,
            5,
            RelayHealth.Ok(0L),
            InternetDeliveryService.SHORE_PASS,
        )
        assertEquals(CoreConnectionHealth.NEEDS_ATTENTION, stopped.health)
        assertEquals(MeshStatusDotColor.AMBER, stopped.dot)

        val radioOff = build(
            MeshRuntimeState.NO_BLUETOOTH,
            0,
            RelayHealth.Ok(0L),
            InternetDeliveryService.SHORE_PASS,
        )
        assertEquals(CoreConnectionHealth.LIMITED, radioOff.health)
        assertEquals(MeshStatusDotColor.AMBER, radioOff.dot)
    }

    @Test
    fun `starting up makes no claim at all while the check is still inside its bound`() {
        val status = build(
            MeshRuntimeState.STARTING,
            0,
            RelayHealth.Checking,
            InternetDeliveryService.SHORE_PASS,
            checkingSinceMs = NOW - 1_000L,
        )
        assertEquals(CoreConnectionHealth.CHECKING, status.health)
        assertEquals(MeshStatusDotColor.NEUTRAL, status.dot)
    }

    @Test
    fun `the pill and the connection details card cannot disagree`() {
        // Not a re-implementation to compare against: the same core call the
        // page makes, on the same mapped inputs. What is pinned is that the
        // pill hands the core everything it is given and reports the answer
        // rather than a second opinion formed on the way past.
        val relayStates = listOf(
            RelayHealth.Ok(0L),
            RelayHealth.NoInternet,
            RelayHealth.Failing(0L),
            RelayHealth.Expired(0L),
            RelayHealth.ExpiredReadOnly(0L),
            RelayHealth.Suspended(0L),
            RelayHealth.TokenRejected(0L),
            RelayHealth.QuotaFull(0L),
            RelayHealth.RateLimited(0L),
        )
        for (runtime in MeshRuntimeState.entries) {
            for (relayHealth in relayStates) {
                for (lanListening in listOf(true, false)) {
                    for (nearby in listOf(0, 3)) {
                        val pill = build(
                            runtime,
                            nearby,
                            relayHealth,
                            InternetDeliveryService.SHORE_PASS,
                            lanListening = lanListening,
                        )
                        val page = coreClassifyConnectionHealth(
                            CoreConnectionHealthInput(
                                runtime = ConnectionInputs.runtime(runtime),
                                bluetooth = ConnectionInputs.bluetooth(runtime),
                                bluetoothLinks = 0u,
                                localWifi = ConnectionInputs.localWifi(runtime, lanListening),
                                localWifiLinks = 0u,
                                relay = ConnectionInputs.relay(relayHealth, true),
                                validatedInternet =
                                ConnectionInputs.validatedInternet(relayHealth),
                                nearbyFriendCount = nearby.toUInt(),
                                checkingSinceMs = NO_CHECK,
                                nowMs = NOW,
                            ),
                        )
                        assertEquals(
                            "$runtime / $relayHealth / lan=$lanListening / nearby=$nearby",
                            page.state,
                            pill.health,
                        )
                    }
                }
            }
        }
    }

    @Test
    fun `the mapped inputs are the page's, not a private set`() {
        // A guard on the mapping itself: if the pill ever started deciding
        // what the relay path is, this is where it would show.
        assertEquals(
            CoreRelayPathState.NOT_SET_UP,
            ConnectionInputs.relay(RelayHealth.NoConfig, false),
        )
        assertEquals(CoreMeshRuntime.ACTIVE, ConnectionInputs.runtime(MeshRuntimeState.ACTIVE))
        assertEquals(
            CoreDirectPathState.OFF,
            ConnectionInputs.localWifi(MeshRuntimeState.ACTIVE, listening = false),
        )
    }
}
