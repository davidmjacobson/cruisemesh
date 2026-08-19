package com.cruisemesh.app.relay

import com.cruisemesh.app.mesh.HostCoreLibrary
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.DeviceKeypair
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.InboxKey
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.RelayRotationNextStep
import uniffi.cruisemesh_core.RevocationCommit
import uniffi.cruisemesh_core.coreLinkGenesisRoster
import uniffi.cruisemesh_core.coreLinkSignNewDeviceRoster
import uniffi.cruisemesh_core.coreRevokeDevicesRoster
import uniffi.cruisemesh_core.generateDeviceKeypair
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.relayDepositTokenFor
import java.io.IOException

/**
 * §10 step 2's driver, against a real core and a relay that answers whatever
 * the test needs it to.
 *
 * The properties pinned here are the ones a rotation cannot be allowed to lose,
 * in the order they would hurt:
 *
 * 1. **The journal is written before the call.** A client that asked first and
 *    wrote afterwards would, on one dropped response, hold neither credential
 *    and lock a family out of its own mailbox.
 * 2. **No answer, no commit.** The saved credential moves only when the relay
 *    has confirmed the re-key.
 * 3. **An unreachable relay does not lose the rotation.** The removal is
 *    already done; the rotation waits in the journal and a later pass — in a
 *    later process, if it comes to that — performs it.
 * 4. **Nothing hot-loops.** There is a rate-limit incident in this codebase's
 *    history, and the rotate route is the one a family needs on the day a
 *    phone is stolen.
 */
class RelayRotationDriverTest {

    companion object {
        private const val NOW = 1_755_000_000_000L
        private const val RELAY_URL = "https://relay.example"
        private const val OLD_TOKEN = "family-token-from-before-the-removal"

        init {
            HostCoreLibrary.load()
        }
    }

    // -- The fixtures -------------------------------------------------------

    /** One person, two phones, and the revocation that buries the second. */
    private class Fleet {
        val identity: Identity = generateIdentity()
        val approver: DeviceKeypair = generateDeviceKeypair()
        val sibling: DeviceKeypair = generateDeviceKeypair()
        val store: MessageStore = MessageStore.open(":memory:")
        val revocation: RevocationCommit

        init {
            val genesis = coreLinkGenesisRoster(identity.signSk, approver.signPk, approver.agreePk)
            val roster = coreLinkSignNewDeviceRoster(
                genesis,
                identity.signPk,
                approver.signSk,
                sibling.signPk,
                sibling.agreePk,
            ).roster
            store.adoptOwnRoster(roster, identity.signPk, approver.deviceId)
            store.coreSetOwnSyncContext(roster, roster.inboxKeyGeneration)
            // Generation 0 is the deployed person agreement key (§10 note 4).
            val key = InboxKey(0uL, identity.agreePk, identity.agreeSk)
            val update = coreRevokeDevicesRoster(
                roster,
                identity.signPk,
                approver.signSk,
                listOf(sibling.deviceId),
                key,
            )
            store.beginOwnRevocation(update, identity.signPk, approver, NOW)
            revocation = store.commitOwnRevocation(update, identity.signPk, approver, key, NOW)
        }
    }

    private class SavedPass(var config: RelayConfig?) : RelayRotationCredential {
        var epoch: Long = 0L
        var adoptions: Int = 0

        override fun current(): RelayConfig? = config

        override fun epoch(): Long = epoch

        override fun adopt(config: RelayConfig) {
            this.config = config
            // T23: adopting is an endpoint change, and the epoch climbs -- which
            // is what makes the next pass fan the new deposit token out.
            epoch += 1
            adoptions += 1
        }
    }

    /** A relay that answers however the test says, and counts being asked. */
    private class Relay(val answer: (String, Int) -> ByteArray) {
        val bearers = mutableListOf<String>()

        fun rotate(config: RelayConfig, @Suppress("UNUSED_PARAMETER") body: ByteArray): ByteArray {
            bearers += config.relayToken
            return answer(config.relayToken, bearers.size)
        }
    }

    private fun rotatedBody(token: String, envelopesMoved: Int = 0, rotated: Boolean = true): ByteArray =
        (
            """{"family_token":"$token","deposit_token":"${relayDepositTokenFor(token)}",""" +
                """"envelopes_moved":$envelopesMoved,"rotated":$rotated}"""
            ).toByteArray()

    /** What the surface would be told: null until the driver says either way. */
    private class Notice {
        var blocked: Boolean? = null

        fun record(value: Boolean) {
            blocked = value
        }
    }

    private fun driver(
        fleet: Fleet,
        pass: SavedPass,
        relay: Relay,
        pacer: RelayRotationPacer,
        clock: () -> Long,
        notice: Notice = Notice(),
        onRotated: () -> Unit = {},
    ) = RelayRotationDriver(
        store = fleet.store,
        credential = pass,
        rotate = relay::rotate,
        onRotated = onRotated,
        notice = notice::record,
        pacer = pacer,
        clock = clock,
    )

    // -- The tests ----------------------------------------------------------

    @Test
    fun `the rotation is written down before the call and committed only after it`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        var pendingWhenAsked: String? = null
        val relay = Relay { bearer, _ ->
            // The journal must already name the replacement by the time the
            // relay is asked; that row is the only thing that survives a lost
            // answer.
            pendingWhenAsked = fleet.store.pendingRelayRotation()?.newToken
            assertEquals("the retired credential is presented first", OLD_TOKEN, bearer)
            rotatedBody(pendingWhenAsked!!, envelopesMoved = 7)
        }
        var nudges = 0
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW }) { nudges += 1 }

        assertTrue(driver.begin(fleet.revocation))
        val planned = fleet.store.pendingRelayRotation()
        assertNotNull("the removal wrote the rotation down without any network", planned)
        assertEquals(OLD_TOKEN, planned!!.supersededToken)
        assertEquals("the credential does not move until the relay confirms", OLD_TOKEN, pass.config!!.relayToken)

        val outcome = driver.rotateIfPending(fleet.identity)

        assertTrue(outcome is RelayRotationOutcome.Rotated)
        assertEquals(7uL, (outcome as RelayRotationOutcome.Rotated).envelopesMoved)
        assertFalse(outcome.alreadyDone)
        assertEquals(planned.newToken, pendingWhenAsked)
        assertEquals(planned.newToken, pass.config!!.relayToken)
        assertEquals(RELAY_URL, pass.config!!.relayUrl)
        assertEquals(1, pass.adoptions)
        assertEquals("the endpoint change has to reach contacts", 1, nudges)
        assertNull("a committed rotation is not re-run", fleet.store.pendingRelayRotation())
        // §10.2's own-device leg: the replacement is in the shared settings for
        // a sibling that slept through the ceremony.
        assertEquals(planned.newToken, fleet.store.relayCredentialSetting()!!.token)
    }

    @Test
    fun `a removal with no reachable relay still removes and the rotation waits`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        var reachable = false
        val relay = Relay { bearer, _ ->
            if (!reachable) throw IOException("no route to host")
            rotatedBody(fleet.store.pendingRelayRotation()!!.newToken).also {
                assertEquals(OLD_TOKEN, bearer)
            }
        }
        val pacer = RelayRotationPacer()
        var now = NOW
        val offline = driver(fleet, pass, relay, pacer, { now })

        assertTrue(offline.begin(fleet.revocation))
        val deferred = offline.rotateIfPending(fleet.identity)
        assertTrue(deferred is RelayRotationOutcome.Deferred)
        assertTrue((deferred as RelayRotationOutcome.Deferred).step is RelayRotationNextStep.Retry)
        val planned = fleet.store.pendingRelayRotation()
        assertNotNull("an unreachable relay must not lose the rotation", planned)
        assertEquals(OLD_TOKEN, pass.config!!.relayToken)

        // A later process, on a phone that has since found internet. The pacer
        // is per-process and the journal is not, which is exactly the split
        // this asserts: nothing about the retry depends on remembering.
        reachable = true
        now += 60_000
        val relaunched = driver(fleet, pass, relay, RelayRotationPacer(), { now })
        val outcome = relaunched.rotateIfPending(fleet.identity)

        assertTrue(outcome is RelayRotationOutcome.Rotated)
        assertEquals(planned!!.newToken, pass.config!!.relayToken)
        assertNull(fleet.store.pendingRelayRotation())
    }

    @Test
    fun `a refused call never commits`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ -> throw RelayHttpException(500, null, "relay is having a day") }
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW })

        driver.begin(fleet.revocation)
        val outcome = driver.rotateIfPending(fleet.identity)

        assertTrue(outcome is RelayRotationOutcome.Deferred)
        assertNotNull(fleet.store.pendingRelayRotation())
        assertEquals(OLD_TOKEN, pass.config!!.relayToken)
        assertEquals(0, pass.adoptions)
        assertNull("nothing was announced to the siblings either", fleet.store.relayCredentialSetting())
    }

    /**
     * The recovery case the whole two-token design exists for: the rotation
     * landed, the answer did not come back, and the device wakes up holding a
     * credential the server has already retired.
     */
    @Test
    fun `a credential the relay has already retired is confirmed under the replacement`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { bearer, attempt ->
            if (attempt == 1) {
                assertEquals(OLD_TOKEN, bearer)
                throw RelayHttpException(401, null, "unknown family token")
            }
            // relayd answers a repeat presentation with the same values and
            // `rotated: false`, which is a success, not a failure.
            assertEquals(fleet.store.pendingRelayRotation()!!.newToken, bearer)
            rotatedBody(bearer, rotated = false)
        }
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW })

        driver.begin(fleet.revocation)
        val planned = fleet.store.pendingRelayRotation()!!
        val outcome = driver.rotateIfPending(fleet.identity)

        assertTrue(outcome is RelayRotationOutcome.Rotated)
        assertTrue((outcome as RelayRotationOutcome.Rotated).alreadyDone)
        assertEquals(listOf(OLD_TOKEN, planned.newToken), relay.bearers)
        assertEquals(planned.newToken, pass.config!!.relayToken)
        assertNull(fleet.store.pendingRelayRotation())
    }

    @Test
    fun `a rate limited rotation waits out the window instead of hammering it`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ ->
            throw RelayHttpException(429, "rate_limited", "too fast", retryAfter = "60")
        }
        val pacer = RelayRotationPacer()
        var now = NOW
        val driver = driver(fleet, pass, relay, pacer, { now })

        driver.begin(fleet.revocation)
        driver.rotateIfPending(fleet.identity)
        assertEquals(1, relay.bearers.size)

        // Every pass for the next several minutes finds the rotation owed and
        // does not make a request. This is the behaviour a rerun loop that
        // ignored Retry-After once cost a family ~290 posts a minute for.
        var passes = 0
        while (passes < 8) {
            now += 60_000
            assertTrue(driver.rotateIfPending(fleet.identity) is RelayRotationOutcome.Waiting)
            passes += 1
        }
        assertEquals(1, relay.bearers.size)
        assertNotNull("and it is still owed", fleet.store.pendingRelayRotation())

        // Past the window, it is tried again -- once.
        now = pacer.nextAttemptAtMs
        driver.rotateIfPending(fleet.identity)
        assertEquals(2, relay.bearers.size)
    }

    @Test
    fun `a relay that cannot re-key from a device is not asked forever`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ ->
            throw RelayHttpException(409, "rotation_unsupported", "token is configured on the server")
        }
        var now = NOW
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { now })

        driver.begin(fleet.revocation)
        val outcome = driver.rotateIfPending(fleet.identity)

        assertTrue(outcome is RelayRotationOutcome.GaveUp)
        assertEquals(
            RelayRotationNextStep.ServerManagedToken,
            (outcome as RelayRotationOutcome.GaveUp).step,
        )
        // Cleared, so the next pass does not ask again. The person keeps the
        // credential they have -- and so, honestly, does the removed device.
        assertNull(fleet.store.pendingRelayRotation())
        assertEquals(OLD_TOKEN, pass.config!!.relayToken)
        now += 3_600_000
        assertEquals(
            RelayRotationOutcome.NothingPending,
            driver.rotateIfPending(fleet.identity),
        )
        assertEquals(1, relay.bearers.size)
    }

    @Test
    fun `a person with no shore pass has nothing to rotate`() {
        val fleet = Fleet()
        val pass = SavedPass(null)
        val relay = Relay { _, _ -> throw AssertionError("no pass, no call") }
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW })

        assertFalse(driver.begin(fleet.revocation))
        assertNull(fleet.store.pendingRelayRotation())
        assertEquals(
            RelayRotationOutcome.NothingPending,
            driver.rotateIfPending(fleet.identity),
        )
    }

    /**
     * §10.2's own-device leg from the other end: a sibling that was asleep
     * through the ceremony reads the replacement out of the shared settings and
     * writes it down.
     *
     * Driven here through a store that has the setting in it, because the
     * transport that would deliver it does not exist on either shell yet. What
     * the test is really pinning is the guard around the adoption, since that
     * is what will still be load-bearing when the transport lands.
     */
    @Test
    fun `a sibling adopts an announced credential only on its own relay`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ -> throw AssertionError("adopting makes no calls") }
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW })

        // Nothing announced: nothing changes.
        driver.adoptAnnouncedCredential()
        assertEquals(OLD_TOKEN, pass.config!!.relayToken)

        // The rotating sibling's announcement, as it lands in the settings.
        driver.begin(fleet.revocation)
        val planned = fleet.store.pendingRelayRotation()!!
        fleet.store.commitRelayRotation(planned, NOW)
        // Put this device back where a sibling that missed the ceremony is:
        // holding the retired credential, with the replacement announced.
        pass.config = RelayConfig(RELAY_URL, OLD_TOKEN)

        driver.adoptAnnouncedCredential()
        assertEquals(planned.newToken, pass.config!!.relayToken)

        // A phone on a different relay is not the family's to move, and a phone
        // whose person removed the pass must not have one reinstalled.
        pass.config = RelayConfig("https://relay.somewhere-else.example", OLD_TOKEN)
        driver.adoptAnnouncedCredential()
        assertEquals(OLD_TOKEN, pass.config!!.relayToken)
        pass.config = null
        driver.adoptAnnouncedCredential()
        assertNull(pass.config)
    }

    /**
     * A second removal while a rotation is still in flight must not re-mint.
     * The pending row may already name the credential the server moved to, and
     * overwriting it would throw away the only record of it — locking the
     * family out of its own mailbox to lock one thief out.
     */
    @Test
    fun `a second removal lets the rotation already in flight finish`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ -> throw IOException("offline") }
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW })

        driver.begin(fleet.revocation)
        val first = fleet.store.pendingRelayRotation()!!.newToken

        assertTrue(driver.begin(fleet.revocation))
        assertEquals(first, fleet.store.pendingRelayRotation()!!.newToken)
    }

    /**
     * The removal happened at sea and the pass changed before the relay was
     * ever reachable. The rotation is about a family this device has left, so
     * it must be dropped rather than performed: the call would re-key somebody
     * else's family, and committing it would write that family's credential
     * over the pass this person is actually on.
     */
    @Test
    fun `a rotation planned against a pass this device has left is dropped, not performed`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ -> throw AssertionError("a pass this device left is never asked") }
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW })

        driver.begin(fleet.revocation)
        assertNotNull(fleet.store.pendingRelayRotation())

        // Ashore: a setup card for a different family.
        pass.config = RelayConfig("https://relay.elsewhere.example", "a-different-familys-token")
        assertEquals(
            RelayRotationOutcome.NothingPending,
            driver.rotateIfPending(fleet.identity),
        )
        assertNull("the stale row does not sit there asking forever", fleet.store.pendingRelayRotation())
        assertEquals("a-different-familys-token", pass.config!!.relayToken)
        assertEquals("https://relay.elsewhere.example", pass.config!!.relayUrl)
        assertEquals(0, pass.adoptions)
    }

    /** And the same for a pass the person deliberately removed. */
    @Test
    fun `a rotation is not performed for a pass the person has cleared`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ -> throw AssertionError("no pass, no call") }
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW })

        driver.begin(fleet.revocation)
        pass.config = null

        assertEquals(
            RelayRotationOutcome.NothingPending,
            driver.rotateIfPending(fleet.identity),
        )
        assertNull(fleet.store.pendingRelayRotation())
        assertNull("a removed Shore Pass is never reinstalled by a rotation", pass.config)
    }

    /**
     * The other half of the same reconciliation: a removal made *after* the
     * pass changed must plan a rotation of the credential the removed device
     * actually holds, not report the stale row as one already queued.
     */
    @Test
    fun `a removal after the pass changed re-plans against the pass in use`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ -> throw IOException("offline") }
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW })

        driver.begin(fleet.revocation)
        val stale = fleet.store.pendingRelayRotation()!!.newToken

        pass.config = RelayConfig(RELAY_URL, "the-token-this-device-now-holds")
        assertTrue(driver.begin(fleet.revocation))
        val replanned = fleet.store.pendingRelayRotation()!!
        assertEquals("the-token-this-device-now-holds", replanned.supersededToken)
        assertTrue("a fresh replacement, not the stale one", replanned.newToken != stale)
    }

    /**
     * A relay that will never re-key from this device is not just stopped, it
     * is *said*. The removal confirmation promised the removed phone loses the
     * family mailbox, and a promise the app privately gave up on is worse than
     * one it never made.
     */
    @Test
    fun `a refusal that can never succeed is written down for the surface`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ ->
            throw RelayHttpException(403, "rotation_unauthorized", "somebody else holds the key")
        }
        val notice = Notice()
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW }, notice = notice)

        driver.begin(fleet.revocation)
        assertEquals("planning a rotation is a fresh promise", false, notice.blocked)

        val outcome = driver.rotateIfPending(fleet.identity)
        assertTrue(outcome is RelayRotationOutcome.GaveUp)
        assertEquals(
            RelayRotationNextStep.NotTheAuthority,
            (outcome as RelayRotationOutcome.GaveUp).step,
        )
        assertEquals(true, notice.blocked)
    }

    /** And a rotation that lands takes the note away again. */
    @Test
    fun `a rotation that lands clears the note`() {
        val fleet = Fleet()
        val pass = SavedPass(RelayConfig(RELAY_URL, OLD_TOKEN))
        val relay = Relay { _, _ -> rotatedBody(fleet.store.pendingRelayRotation()!!.newToken) }
        val notice = Notice()
        notice.blocked = true
        val driver = driver(fleet, pass, relay, RelayRotationPacer(), { NOW }, notice = notice)

        driver.begin(fleet.revocation)
        assertTrue(driver.rotateIfPending(fleet.identity) is RelayRotationOutcome.Rotated)
        assertEquals(false, notice.blocked)
    }
}
