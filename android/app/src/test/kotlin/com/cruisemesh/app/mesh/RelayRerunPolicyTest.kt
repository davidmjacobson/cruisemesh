package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.coreFamilyRelayRerunVectors

/**
 * The rerun decision lives in the core
 * (`core/src/session/relay_policy.rs::core_relay_rerun_action`), under
 * `RATE-01`. This file proves Android's shim reaches it and maps the answer
 * onto the enum `RelaySyncEngine`'s `when` reads -- including the storm case
 * the rule exists for, which the core exports as a named vector rather than
 * leaving each platform to re-type it.
 */
class RelayRerunPolicyTest {

    @Test
    fun `every core rerun vector survives the shim`() {
        for (vector in coreFamilyRelayRerunVectors()) {
            assertEquals(
                vector.name,
                vector.expected,
                relayRerunAction(
                    pendingRequested = vector.pendingRequested,
                    canSync = vector.canSync,
                    backoffRemainingMs = vector.backoffRemainingMs,
                ),
            )
        }
    }

    @Test
    fun `the engine's three branches are all reachable`() {
        // The `when` in RelaySyncEngine is exhaustive over these three, and an
        // enum discriminant that arrived scrambled from the FFI would land the
        // pass in the wrong branch without failing to compile.
        val actions = coreFamilyRelayRerunVectors().map { vector ->
            relayRerunAction(vector.pendingRequested, vector.canSync, vector.backoffRemainingMs)
        }
        assertEquals(RelayRerunAction.entries.toSet(), actions.toSet())
    }
}
