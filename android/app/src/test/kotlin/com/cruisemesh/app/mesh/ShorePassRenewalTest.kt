package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreFamilyPassState
import uniffi.cruisemesh_core.CoreFamilyStatus
import uniffi.cruisemesh_core.relayDepositTokenFor

/**
 * The renewal path's shell-side rules: what the app links to, and when it
 * says anything at all. The date rule itself is the core's
 * (`relay_pass_delivery_through_ms`, pinned in `relay_wire.rs`); what is
 * Android's to prove is that a not-read-yet status is the same silence as no
 * end date, and that the link the app hands the browser is the one the site
 * can resolve.
 */
class ShorePassRenewalTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }
    }

    private val now = 1_800_000_000_000L
    private val memberToken = "b4c1f0a95e2d47318af6c0d21e7b9a83"

    private fun status(
        expiresMs: Long?,
        state: CoreFamilyPassState = CoreFamilyPassState.ACTIVE,
    ) = CoreFamilyStatus(plan = "shore", expiresMs = expiresMs, state = state)

    @Test
    fun `the family token rides the fragment, never the query`() {
        val url = shorePassRenewUrl(memberToken)
        assertEquals("https://cruisemesh.app/renew/app#f=$memberToken", url)
        // The invariant this whole shape exists for: a fragment is never sent
        // to a server, so the credential stays out of every log on the way.
        assertFalse("the token must not ride a query parameter", url!!.contains("?"))
    }

    @Test
    fun `surrounding whitespace never reaches the link`() {
        assertEquals(shorePassRenewUrl(memberToken), shorePassRenewUrl("  $memberToken\n"))
    }

    @Test
    fun `there is no link when there is no token the site could resolve`() {
        assertNull(shorePassRenewUrl(""))
        assertNull(shorePassRenewUrl("   "))
        // A deposit credential is the post-only attenuation friend cards
        // carry, not the family token a purchase row is keyed by.
        assertNull(shorePassRenewUrl(relayDepositTokenFor(memberToken)))
        // Anything that would have to be escaped is refused rather than
        // encoded: one place to disagree about the token is one too many.
        assertNull(shorePassRenewUrl("token with spaces"))
        assertNull(shorePassRenewUrl("token#f=other"))
        assertNull(shorePassRenewUrl("token&next=evil"))
    }

    @Test
    fun `an unread status says exactly what a pass with no end date says`() {
        assertNull(shorePassDeliveryThroughMs(null, now))
        assertNull(shorePassDeliveryThroughMs(status(expiresMs = null), now))
    }

    @Test
    fun `a future end date is shown and a past one is not`() {
        assertEquals(now + 1, shorePassDeliveryThroughMs(status(now + 1), now))
        assertNull(shorePassDeliveryThroughMs(status(now - 1, CoreFamilyPassState.GRACE), now))
    }

    @Test
    fun `renewal is offered while a date is still ahead, and once it has run out`() {
        assertTrue(shorePassOffersRenewal(RelayHealth.Ok(now), deliveryThroughMs = now + 1))
        assertTrue(shorePassOffersRenewal(RelayHealth.Expired(now), deliveryThroughMs = null))
    }

    @Test
    fun `renewal is not offered where paying again would not help`() {
        // No end date: nothing to renew. This is the self-hosted relay, and
        // the phone that has simply not read its status yet.
        assertFalse(shorePassOffersRenewal(RelayHealth.Ok(now), deliveryThroughMs = null))
        // A suspension is not lifted by paying, and a rejected setup card is a
        // different problem with its own instructions.
        assertFalse(shorePassOffersRenewal(RelayHealth.Suspended(now), deliveryThroughMs = null))
        assertFalse(shorePassOffersRenewal(RelayHealth.TokenRejected(now), deliveryThroughMs = null))
        // Being offline is never a reason to sell someone anything.
        assertFalse(shorePassOffersRenewal(RelayHealth.NoInternet, deliveryThroughMs = null))
    }
}
