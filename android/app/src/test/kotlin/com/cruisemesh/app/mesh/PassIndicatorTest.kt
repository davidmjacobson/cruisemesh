package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test

class PassIndicatorTest {
    private val now = 1_800_000_000_000L

    @Test
    fun `no saved pass shows nothing regardless of health`() {
        // The free tier is not a fault: nearby delivery works without a pass,
        // so an un-configured phone must not wear an error mark.
        val healths = listOf(
            RelayHealth.NoConfig,
            RelayHealth.Checking,
            RelayHealth.NoInternet,
            RelayHealth.Ok(now),
            RelayHealth.Failing(now),
            RelayHealth.Expired(now),
            RelayHealth.Suspended(now),
            RelayHealth.TokenRejected(now),
        )
        for (health in healths) {
            assertEquals(
                "unconfigured phone should show no indicator for $health",
                PassIndicator.NONE,
                passIndicator(health, configured = false),
            )
        }
    }

    @Test
    fun `a working pass reads ready`() {
        assertEquals(PassIndicator.READY, passIndicator(RelayHealth.Ok(now), configured = true))
    }

    @Test
    fun `being offline is never an error`() {
        // Regression guard for the whole point of this indicator: CruiseMesh
        // is used at sea, so "no internet" is the expected state and must not
        // be styled as a failure.
        assertEquals(
            PassIndicator.WAITING,
            passIndicator(RelayHealth.NoInternet, configured = true),
        )
    }

    @Test
    fun `a reachable-but-unhappy relay is amber, not red`() {
        assertEquals(
            PassIndicator.ATTENTION,
            passIndicator(RelayHealth.Failing(now), configured = true),
        )
    }

    @Test
    fun `states that need the user to act are red`() {
        for (health in listOf(
            RelayHealth.Expired(now),
            RelayHealth.Suspended(now),
            RelayHealth.TokenRejected(now),
        )) {
            assertEquals(
                "$health will not self-heal and should demand action",
                PassIndicator.ACTION_REQUIRED,
                passIndicator(health, configured = true),
            )
        }
    }

    @Test
    fun `an unchecked saved pass shows nothing rather than flickering`() {
        for (health in listOf(RelayHealth.NoConfig, RelayHealth.Checking)) {
            assertEquals(
                PassIndicator.NONE,
                passIndicator(health, configured = true),
            )
        }
    }
}
