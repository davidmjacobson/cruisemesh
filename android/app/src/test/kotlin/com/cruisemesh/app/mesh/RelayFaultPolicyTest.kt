package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.CoreRelayFault
import uniffi.cruisemesh_core.CoreRelayPassHealth
import uniffi.cruisemesh_core.coreFamilyRelayHealthVectors
import uniffi.cruisemesh_core.relayFaultIsTransient

/**
 * The pass health fold lives in the core
 * (`core/src/session/relay_policy.rs`), and its precedence -- why an expired
 * pass beats a successful poll and a suspended one does not -- is pinned
 * there. What is Android's to prove is the projection: that every core health
 * reaches the right [RelayHealth] with this shell's timestamp on it, and that
 * the indicator the Shore Pass screen draws still follows the core's
 * transient/persistent split.
 */
class RelayFaultPolicyTest {
    private val now = 1_800_000_000_000L

    private fun expected(health: CoreRelayPassHealth): RelayHealth = when (health) {
        CoreRelayPassHealth.OK -> RelayHealth.Ok(now)
        CoreRelayPassHealth.QUOTA_FULL -> RelayHealth.QuotaFull(now)
        CoreRelayPassHealth.MESSAGE_TOO_LARGE -> RelayHealth.MessageTooLarge(now)
        CoreRelayPassHealth.RATE_LIMITED -> RelayHealth.RateLimited(now)
        CoreRelayPassHealth.EXPIRED -> RelayHealth.Expired(now)
        CoreRelayPassHealth.EXPIRED_READ_ONLY -> RelayHealth.ExpiredReadOnly(now)
        CoreRelayPassHealth.SUSPENDED -> RelayHealth.Suspended(now)
        CoreRelayPassHealth.TOKEN_REJECTED -> RelayHealth.TokenRejected(now)
        CoreRelayPassHealth.FAILING -> RelayHealth.Failing(now)
    }

    @Test
    fun `every core health vector projects onto the shell's health`() {
        for (vector in coreFamilyRelayHealthVectors()) {
            assertEquals(
                vector.name,
                expected(vector.expected),
                relayHealthAfterSyncPass(
                    vector.fault,
                    ownRelaySucceeded = vector.ownRelaySucceeded,
                    anyRelaySucceeded = vector.anyRelaySucceeded,
                    now = now,
                ),
            )
        }
    }

    @Test
    fun `every core health has a distinct projection`() {
        // A `when` branch pointing at the wrong RelayHealth would still
        // compile and would still be exhaustive. Two healths collapsing onto
        // one display state is the shape that bug takes.
        val projections = CoreRelayPassHealth.entries.map { expected(it) }
        assertEquals(CoreRelayPassHealth.entries.size, projections.toSet().size)
    }

    @Test
    fun `the worse-fault fold keeps the persistent condition in either order`() {
        // Order independence is a property of the fold, and the fold is what
        // the engine calls repeatedly as a pass observes faults. Asserted
        // through the shim because that is the call the engine makes.
        var fault: CoreRelayFault? = null
        fault = worseRelayFault(fault, CoreRelayFault.RATE_LIMITED)
        fault = worseRelayFault(fault, CoreRelayFault.MAILBOX_FULL)
        assertEquals(CoreRelayFault.MAILBOX_FULL, fault)

        fault = null
        fault = worseRelayFault(fault, CoreRelayFault.MAILBOX_FULL)
        fault = worseRelayFault(fault, CoreRelayFault.RATE_LIMITED)
        assertEquals(CoreRelayFault.MAILBOX_FULL, fault)
    }

    @Test
    fun `indicator buckets agree with the core's transient split`() {
        // Presentation, and therefore genuinely this shell's: the "?" state is
        // exactly the core's transient set, and everything the core calls
        // persistent renders as "!". Pins the rendering to the policy so they
        // cannot drift apart.
        for (fault in listOf(
            CoreRelayFault.PASS_EXPIRED,
            CoreRelayFault.PASS_SUSPENDED,
            CoreRelayFault.TOKEN_REJECTED,
            CoreRelayFault.MAILBOX_FULL,
            CoreRelayFault.MESSAGE_TOO_LARGE,
            CoreRelayFault.RATE_LIMITED,
        )) {
            val health = relayHealthAfterSyncPass(
                fault,
                ownRelaySucceeded = false,
                anyRelaySucceeded = false,
                now = now,
            )
            val indicator = passIndicator(health, configured = true)
            if (relayFaultIsTransient(fault)) {
                assertEquals(
                    "$fault is transient and must stay amber",
                    PassIndicator.ATTENTION,
                    indicator,
                )
            } else {
                assertEquals(
                    "$fault is persistent and must demand action",
                    PassIndicator.ACTION_REQUIRED,
                    indicator,
                )
            }
        }
    }
}
