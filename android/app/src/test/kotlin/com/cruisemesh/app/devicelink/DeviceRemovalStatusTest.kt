package com.cruisemesh.app.devicelink

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreLinkActivationStage
import uniffi.cruisemesh_core.DeviceKeypair
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.InboxKey
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.RevocationAdoptionOutcome
import uniffi.cruisemesh_core.Roster
import uniffi.cruisemesh_core.coreEncodeRoster
import uniffi.cruisemesh_core.coreLinkGenesisRoster
import uniffi.cruisemesh_core.coreLinkSignNewDeviceRoster
import uniffi.cruisemesh_core.coreRevokeDevicesRoster
import uniffi.cruisemesh_core.generateDeviceKeypair
import uniffi.cruisemesh_core.generateIdentity

/**
 * §10 step 5 as the removed phone lives it, from this shell's side.
 *
 * The field session on 2026-08-18 is the scenario, in full: the approving phone
 * removes a device; four minutes later, on the same Wi-Fi, the removed phone
 * still holds the old device list, still says the mesh is on, and still behaves
 * as though it is linked. With no contacts and no Shore Pass there was no
 * indirect signal either, so waiting longer would never have fixed it.
 *
 * What is pinned here is the shell half: the two process-wide answers the
 * screens and the radios read must both flip on the notice, and they must not
 * flip for a document that does not deserve it.
 */
class DeviceRemovalStatusTest {

    @After
    fun tearDown() {
        // Both objects are process-wide singletons, so a test that left one
        // set would carry into the next. Put them back the way a fresh install
        // reads them.
        val fresh = MessageStore.open(":memory:")
        DeviceRemovalStatus.refresh(fresh)
        LinkVisibility.unregister()
        LinkVisibility.refresh(fresh)
    }

    @Test
    fun `an install that was never removed says so`() {
        val fleet = Fleet.link()

        DeviceRemovalStatus.refresh(fleet.removedStore)

        assertFalse(DeviceRemovalStatus.removed.value)
    }

    @Test
    fun `a signed list that buries this phone stands it down`() {
        val fleet = Fleet.link()
        LinkVisibility.refresh(fleet.removedStore)
        assertTrue("linked and quiet before it was told", LinkVisibility.mayAdvertise())

        val adoption = fleet.removedStore.applyOwnRosterNotice(
            coreEncodeRoster(fleet.rosterWithoutTheSecondDevice()),
            fleet.identity.signPk,
            fleet.second.deviceId,
            NOW,
        )

        assertEquals(RevocationAdoptionOutcome.REVOKED_SELF, adoption.outcome)
        // Core's own answer: terminal, and the gate refuses everything from it.
        assertEquals(
            CoreLinkActivationStage.REVOKED,
            fleet.removedStore.linkActivation().stage,
        )
        // The shell's two: the radios go down and the screens change.
        LinkVisibility.refresh(fleet.removedStore)
        assertFalse("a removed device may not advertise", LinkVisibility.mayAdvertise())
        DeviceRemovalStatus.refresh(fleet.removedStore)
        assertTrue(DeviceRemovalStatus.removed.value)
    }

    /**
     * And it stays told. A device ejected in one process must still know on the
     * next launch, which is why the answer is read from the stage rather than
     * remembered as an event.
     */
    @Test
    fun `the answer survives the process that heard it`() {
        val fleet = Fleet.link()
        fleet.removedStore.applyOwnRosterNotice(
            coreEncodeRoster(fleet.rosterWithoutTheSecondDevice()),
            fleet.identity.signPk,
            fleet.second.deviceId,
            NOW,
        )

        DeviceRemovalStatus.refresh(MessageStore.open(":memory:"))
        assertFalse(DeviceRemovalStatus.removed.value)

        DeviceRemovalStatus.refresh(fleet.removedStore)
        assertTrue(DeviceRemovalStatus.removed.value)
    }

    /**
     * The other phone in the fleet reads the same document and is untouched by
     * it: a notice is not a broadcast stop button, it is one person's own list
     * saying who is on it.
     *
     * It does not adopt it either, and that is core's rule rather than an
     * omission: a removal rotates the fleet's inbox key, a plaintext link frame
     * carries no key material, and a sibling that took the list here would hold
     * a fleet whose own traffic it cannot open. §10.1's sealed handoff is what
     * closes that, so the honest answer is "still waiting for the key".
     */
    @Test
    fun `a sibling that is still listed waits for its key instead`() {
        val fleet = Fleet.link()

        val adoption = fleet.approvingStore.applyOwnRosterNotice(
            coreEncodeRoster(fleet.rosterWithoutTheSecondDevice()),
            fleet.identity.signPk,
            fleet.first.deviceId,
            NOW,
        )

        assertEquals(RevocationAdoptionOutcome.AWAITING_ROTATION_KEY, adoption.outcome)
        DeviceRemovalStatus.refresh(fleet.approvingStore)
        assertFalse(DeviceRemovalStatus.removed.value)
    }

    /**
     * A device list nobody's person signed changes nothing. Core is what
     * enforces this -- the shell's link test is the layer above it -- and this
     * says so out loud, because "somebody can post a document that bricks your
     * phone" is exactly the failure this whole mechanism must not introduce.
     */
    @Test
    fun `a list signed by somebody else changes nothing`() {
        val fleet = Fleet.link()
        val stranger = generateIdentity()
        val strangerDevice = generateDeviceKeypair()
        val forged = coreLinkGenesisRoster(
            stranger.signSk,
            strangerDevice.signPk,
            strangerDevice.agreePk,
        )

        runCatching {
            fleet.removedStore.applyOwnRosterNotice(
                coreEncodeRoster(forged),
                fleet.identity.signPk,
                fleet.second.deviceId,
                NOW,
            )
        }.onSuccess { assertEquals(RevocationAdoptionOutcome.REFUSED, it.outcome) }

        assertEquals(
            CoreLinkActivationStage.NOT_LINKING,
            fleet.removedStore.linkActivation().stage,
        )
        DeviceRemovalStatus.refresh(fleet.removedStore)
        assertFalse(DeviceRemovalStatus.removed.value)
    }

    /**
     * One person, two phones, one store each -- §9's ceremony reduced to the
     * documents it produces, which is all this file needs.
     */
    private class Fleet(
        val identity: Identity,
        val first: DeviceKeypair,
        val second: DeviceKeypair,
        val roster: Roster,
        val approvingStore: MessageStore,
        val removedStore: MessageStore,
    ) {
        /** §10.1's update, signed by the phone that holds the signing role. */
        fun rosterWithoutTheSecondDevice(): Roster = coreRevokeDevicesRoster(
            roster,
            identity.signPk,
            first.signSk,
            listOf(second.deviceId),
            // Generation 0 is the deployed person agreement key (§10 note 4),
            // so it is derived rather than stored -- see InboxKeyStore.
            InboxKey(0uL, identity.agreePk, identity.agreeSk),
        ).roster

        companion object {
            fun link(): Fleet {
                val identity = generateIdentity()
                val first = generateDeviceKeypair()
                val second = generateDeviceKeypair()
                val genesis = coreLinkGenesisRoster(identity.signSk, first.signPk, first.agreePk)
                val update = coreLinkSignNewDeviceRoster(
                    genesis,
                    identity.signPk,
                    first.signSk,
                    second.signPk,
                    second.agreePk,
                )
                val approvingStore = MessageStore.open(":memory:")
                approvingStore.adoptOwnRoster(update.roster, identity.signPk, first.deviceId)
                val removedStore = MessageStore.open(":memory:")
                removedStore.adoptOwnRoster(update.roster, identity.signPk, second.deviceId)
                return Fleet(identity, first, second, update.roster, approvingStore, removedStore)
            }
        }
    }

    private companion object {
        const val NOW = 1_755_000_000_000L
    }
}
