package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreFamilyStatus
import uniffi.cruisemesh_core.relayPassDeliveryThroughMs
import uniffi.cruisemesh_core.relayTokenIsDeposit

/**
 * Where a Shore Pass is renewed, and when the app should say so.
 *
 * Pure Kotlin with no Android imports so every rule here is unit-testable
 * directly; the copy lives in `strings.xml`, the date formatting in the
 * Compose layer, and the decision about *which* date is worth showing in the
 * core (`relay_pass_delivery_through_ms`) so both shells say the same thing on
 * the same day, and iOS keeps the rest of these rules alongside its own pass
 * screen.
 */

/**
 * The page that turns a family's pass into a renewal checkout.
 *
 * `/renew/app` rather than the ordinary renewal link because this one is
 * reached from inside the app, with no signed email link to carry: the page
 * identifies the family from the token the app puts on it and starts the same
 * checkout the email link starts.
 */
private const val SHORE_PASS_RENEW_URL_BASE = "https://cruisemesh.app/renew/app"

/**
 * Characters a family token may contain for it to ride a URL fragment
 * unescaped -- RFC 3986's unreserved set.
 *
 * The tokens this app actually holds are hex (`DEPLOY.md` §1) or base64url,
 * both wholly inside it. Anything else is refused rather than escaped: a
 * percent-encoder is a second place for the app and the site to disagree
 * about what the token was, and the failure mode of disagreeing is a renewal
 * page that reports no such pass.
 */
private const val TOKEN_URL_SAFE = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~"

/**
 * The renewal page for this family's pass, or null when there is nothing to
 * link to.
 *
 * The token rides the **fragment**. A fragment is never sent to the server, so
 * it stays out of access logs, proxies and Referer headers on the way -- the
 * same reason friend links carry their payload there. A query parameter would
 * put a live bearer credential in every log between here and the site.
 *
 * Null for a deposit-class credential: that is the post-only attenuation a
 * friend card carries, not the family token a purchase row is keyed by, so a
 * link built from it could only ever reach the failure page.
 */
fun shorePassRenewUrl(familyToken: String): String? {
    val token = familyToken.trim()
    if (token.isEmpty() || relayTokenIsDeposit(token)) return null
    if (token.any { it !in TOKEN_URL_SAFE }) return null
    return "$SHORE_PASS_RENEW_URL_BASE#f=$token"
}

/**
 * When internet delivery runs through, or null for "say nothing" -- no status
 * read yet, no end date, a date already past, or a suspended pass. The rule
 * itself lives in the core; this only tolerates the not-read-yet case the
 * shells have and the core does not.
 */
fun shorePassDeliveryThroughMs(status: CoreFamilyStatus?, nowMs: Long): Long? =
    status?.let { relayPassDeliveryThroughMs(it, nowMs) }

/**
 * Does this pass surface offer to renew?
 *
 * Two occasions, and only two. An expired pass, where renewing is the whole
 * remedy; and a pass with a known end date still ahead of it, where someone
 * looking at that date is exactly the person who might want to act on it.
 *
 * Everything else says nothing. A pass with no end date has nothing to renew,
 * a suspended one is not fixed by paying again, and a rejected setup card is a
 * different problem with its own instructions -- offering renewal for any of
 * those sells someone a thing that will not help them.
 */
fun shorePassOffersRenewal(health: RelayHealth, deliveryThroughMs: Long?): Boolean =
    health is RelayHealth.Expired || deliveryThroughMs != null
