package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreRelayFault
import uniffi.cruisemesh_core.relayClassifyHttpError
import uniffi.cruisemesh_core.relayFaultIsTransient
import uniffi.cruisemesh_core.relayRetryAfterMs

/**
 * CP2b: pins the sync-pass fold ([relayHealthAfterSyncPass] /
 * [worseRelayFault]) and its agreement with the core classification the
 * shells render from. The HTTP-shape -> fault mapping itself is pinned in
 * the core's own tests (core/src/relay_status.rs).
 */
class RelayFaultPolicyTest {
    private val now = 1_800_000_000_000L

    @Test
    fun `structured relayd rejections land in their dedicated health states`() {
        assertEquals(
            RelayHealth.QuotaFull(now),
            relayHealthAfterSyncPass(
                relayClassifyHttpError(507.toUShort(), "family_quota_exceeded"),
                ownRelaySucceeded = false,
                anyRelaySucceeded = false,
                now = now,
            ),
        )
        assertEquals(
            RelayHealth.MessageTooLarge(now),
            relayHealthAfterSyncPass(
                relayClassifyHttpError(413.toUShort(), "envelope_too_large"),
                ownRelaySucceeded = false,
                anyRelaySucceeded = false,
                now = now,
            ),
        )
        assertEquals(
            RelayHealth.RateLimited(now),
            relayHealthAfterSyncPass(
                relayClassifyHttpError(429.toUShort(), "rate_limited"),
                ownRelaySucceeded = false,
                anyRelaySucceeded = false,
                now = now,
            ),
        )
    }

    @Test
    fun `mailbox faults beat a successful poll`() {
        // relayd keeps serving fetches while rejecting posts, so quota /
        // oversized / rate-limited surface even when the rest of the pass
        // succeeded -- exactly the silent-retry-loop CP2b removes.
        assertEquals(
            RelayHealth.QuotaFull(now),
            relayHealthAfterSyncPass(
                CoreRelayFault.MAILBOX_FULL,
                ownRelaySucceeded = true,
                anyRelaySucceeded = true,
                now = now,
            ),
        )
        assertEquals(
            RelayHealth.RateLimited(now),
            relayHealthAfterSyncPass(
                CoreRelayFault.RATE_LIMITED,
                ownRelaySucceeded = true,
                anyRelaySucceeded = true,
                now = now,
            ),
        )
    }

    @Test
    fun `an expired pass beats a successful poll -- the grace window is not health`() {
        // The regression this test exists for: relayd gives an expired
        // family a SEVEN-DAY read-only grace (FAMILY_EXPIRY_GRACE_MS) in
        // which fetch and ack still succeed and only POST takes the 403.
        // So this exact combination -- PASS_EXPIRED noted by the uploads,
        // both success flags true from the polls -- is what a paying family
        // sees for a week after their pass lapses. Folding it to Ok told
        // them the Cruise Pass was working while nothing they wrote left
        // the phone. If this assertion ever reads Ok again, that lie is
        // back.
        assertEquals(
            RelayHealth.Expired(now),
            relayHealthAfterSyncPass(
                CoreRelayFault.PASS_EXPIRED,
                ownRelaySucceeded = true,
                anyRelaySucceeded = true,
                now = now,
            ),
        )
        // Past the grace window relayd rejects reads too, so the flags go
        // false -- the same answer by the other route.
        assertEquals(
            RelayHealth.Expired(now),
            relayHealthAfterSyncPass(
                CoreRelayFault.PASS_EXPIRED,
                ownRelaySucceeded = false,
                anyRelaySucceeded = true,
                now = now,
            ),
        )
        // Our own relay is the expired one; a contact's relay is still
        // reachable. The pass is still expired.
        assertEquals(
            RelayHealth.Expired(now),
            relayHealthAfterSyncPass(
                CoreRelayFault.PASS_EXPIRED,
                ownRelaySucceeded = false,
                anyRelaySucceeded = false,
                now = now,
            ),
        )
    }

    @Test
    fun `the other credential faults keep their pre-CP2b precedence`() {
        // Suspension and token rejection do NOT move up with expiry, and
        // that is load-bearing rather than an oversight: relayd's
        // authorize_family rejects EVERY op for a suspended family and for
        // an unknown token, so neither fault can co-occur with a successful
        // poll at all. Expiry-in-grace is the only credential fault that
        // can, which is why it is the only one that moved.
        assertEquals(
            RelayHealth.Ok(now),
            relayHealthAfterSyncPass(
                CoreRelayFault.PASS_SUSPENDED,
                ownRelaySucceeded = true,
                anyRelaySucceeded = true,
                now = now,
            ),
        )
        assertEquals(
            RelayHealth.Suspended(now),
            relayHealthAfterSyncPass(
                CoreRelayFault.PASS_SUSPENDED,
                ownRelaySucceeded = false,
                anyRelaySucceeded = false,
                now = now,
            ),
        )
        assertEquals(
            RelayHealth.TokenRejected(now),
            relayHealthAfterSyncPass(
                CoreRelayFault.TOKEN_REJECTED,
                ownRelaySucceeded = false,
                anyRelaySucceeded = false,
                now = now,
            ),
        )
    }

    @Test
    fun `no fault means plain success or failure, as before`() {
        assertEquals(
            RelayHealth.Ok(now),
            relayHealthAfterSyncPass(null, ownRelaySucceeded = true, anyRelaySucceeded = true, now = now),
        )
        assertEquals(
            RelayHealth.Failing(now),
            relayHealthAfterSyncPass(null, ownRelaySucceeded = false, anyRelaySucceeded = true, now = now),
        )
    }

    @Test
    fun `worse-fault fold keeps the persistent condition`() {
        // A burst of 507s can also trip the rate limiter in the same pass;
        // the persistent condition is the one worth showing.
        var fault: CoreRelayFault? = null
        fault = worseRelayFault(fault, CoreRelayFault.RATE_LIMITED)
        fault = worseRelayFault(fault, CoreRelayFault.MAILBOX_FULL)
        assertEquals(CoreRelayFault.MAILBOX_FULL, fault)
        // Same observations, opposite order.
        fault = null
        fault = worseRelayFault(fault, CoreRelayFault.MAILBOX_FULL)
        fault = worseRelayFault(fault, CoreRelayFault.RATE_LIMITED)
        assertEquals(CoreRelayFault.MAILBOX_FULL, fault)
    }

    @Test
    fun `indicator buckets agree with the core's transient split`() {
        // The "?" state is exactly the core's transient set; everything the
        // core calls persistent renders as "!". Pins the shell rendering to
        // the core policy so they cannot drift apart.
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

    @Test
    fun `retry-after honors relayd's advertised window`() {
        assertEquals(3_000uL, relayRetryAfterMs("3"))
        assertEquals(60_000uL, relayRetryAfterMs("999"))
        assertEquals(30_000uL, relayRetryAfterMs(null))
        // The transient split is what licenses the quiet backoff: only a
        // self-healing fault may delay sync without telling anyone.
        assertTrue(relayFaultIsTransient(CoreRelayFault.RATE_LIMITED))
        assertFalse(relayFaultIsTransient(CoreRelayFault.MAILBOX_FULL))
    }
}
