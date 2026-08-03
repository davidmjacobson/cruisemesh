package com.cruisemesh.app.friending

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.relayDepositTokenFor

private const val RELAY_URL = "https://relay.example"
private const val OWN_TOKEN = "family-member-token"
private const val TESTER_TOKEN = "tester-pass-member-token"

class FriendDirectoryScopeTest {

    private fun contact(name: String, relayUrl: String? = null, relayToken: String? = null) =
        Contact(
            userId = name.padEnd(16, '.').toByteArray().copyOf(16),
            name = name,
            signPk = ByteArray(32) { 1 },
            agreePk = ByteArray(32) { 2 },
            relayUrl = relayUrl,
            relayToken = relayToken,
            nickname = null,
        )

    /** A card as it is actually issued post-CP4: the family's deposit token. */
    private fun cardFor(name: String, memberToken: String) =
        contact(name, RELAY_URL, relayDepositTokenFor(memberToken))

    /** Default: everyone was met in person, so the pass is what decides. */
    private val allNearby: (ByteArray) -> Boolean = { true }
    private val noneNearby: (ByteArray) -> Boolean = { false }

    private fun candidates(
        recipient: Contact,
        contacts: List<Contact>,
        ownToken: String? = OWN_TOKEN,
        addedNearby: (ByteArray) -> Boolean = allNearby,
    ) = FriendDirectoryScope.candidatesFor(
        recipient = recipient,
        contacts = contacts,
        ownRelayUrl = ownToken?.let { RELAY_URL },
        ownRelayToken = ownToken,
        addedNearby = addedNearby,
    ).map { it.name }

    @Test
    fun `a contact on another pass is never offered and never receives`() {
        val family = cardFor("Sibling", OWN_TOKEN)
        val tester = cardFor("Tester", TESTER_TOKEN)
        val contacts = listOf(family, tester)

        // The reported symptom: a tester-pass person offered inside a family.
        assertEquals(listOf("Sibling"), candidates(cardFor("Kid", OWN_TOKEN), contacts))
        // ...and the same leak outbound, which would hand a family's names to
        // the tester fleet.
        assertEquals(emptyList<String>(), candidates(tester, contacts))
    }

    @Test
    fun `family introductions still work, which is the whole point of the feature`() {
        val contacts = listOf(
            cardFor("Parent", OWN_TOKEN),
            cardFor("Kid1", OWN_TOKEN),
            cardFor("Kid2", OWN_TOKEN),
        )
        assertEquals(listOf("Parent", "Kid2"), candidates(contacts[1], contacts))
    }

    @Test
    fun `a holiday acquaintance without a pass is not family, even met in person`() {
        // The cruise case: another family's kid, scanned face to face, no pass
        // of their own. Being nearby must not buy an exception -- that is
        // exactly how a relative mid-onboarding looks, and letting either one
        // through reopens the propagation the scoping exists to stop.
        val outsider = contact("CruiseKid")
        val parent = cardFor("Parent", OWN_TOKEN)
        assertFalse(
            FriendDirectoryScope.introducible(outsider, RELAY_URL, OWN_TOKEN, addedNearby = true),
        )
        assertEquals(emptyList<String>(), candidates(parent, listOf(outsider, parent)))
    }

    @Test
    fun `a family member joining our pass becomes eligible at that moment`() {
        // Before: no pass, not offered. After: on ours, offered. The
        // pass-change re-fan is what replays this without user action.
        val before = contact("NotSetUpYet")
        val after = cardFor("NotSetUpYet", OWN_TOKEN)
        assertFalse(FriendDirectoryScope.introducible(before, RELAY_URL, OWN_TOKEN, true))
        assertTrue(FriendDirectoryScope.introducible(after, RELAY_URL, OWN_TOKEN, false))
    }

    @Test
    fun `with no pass at all, meeting in person is the only boundary left`() {
        val met = contact("Met")
        val neverMet = contact("NeverMet")
        assertEquals(
            listOf("NeverMet"),
            candidates(
                recipient = met,
                contacts = listOf(met, neverMet),
                ownToken = null,
                addedNearby = allNearby,
            ),
        )
        // A contact added remotely, by somebody else's introduction, stays out.
        assertEquals(
            emptyList<String>(),
            candidates(met, listOf(met, neverMet), ownToken = null, addedNearby = noneNearby),
        )
    }

    @Test
    fun `without a pass, a contact who has one belongs to a family we cannot see`() {
        val passHolder = cardFor("HasPass", TESTER_TOKEN)
        assertFalse(
            FriendDirectoryScope.introducible(passHolder, null, null, addedNearby = true),
        )
    }

    @Test
    fun `a pre-CP4 card carrying the member token itself is still our family`() {
        val legacy = contact("Legacy", RELAY_URL, OWN_TOKEN)
        assertTrue(FriendDirectoryScope.introducible(legacy, RELAY_URL, OWN_TOKEN, false))
    }

    @Test
    fun `the same family token on a different relay host is not our pass`() {
        val elsewhere = contact("Elsewhere", "https://other.example", OWN_TOKEN)
        assertFalse(FriendDirectoryScope.introducible(elsewhere, RELAY_URL, OWN_TOKEN, true))
    }

    @Test
    fun `a recipient is never offered themselves`() {
        val self = cardFor("Kid", OWN_TOKEN)
        assertEquals(emptyList<String>(), candidates(self, listOf(self)))
    }
}
