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

    private fun candidates(
        recipient: Contact,
        contacts: List<Contact>,
        ownToken: String? = OWN_TOKEN,
    ) = FriendDirectoryScope.candidatesFor(
        recipient = recipient,
        contacts = contacts,
        ownRelayUrl = ownToken?.let { RELAY_URL },
        ownRelayToken = ownToken,
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
    fun `a contact without a pass is not introducible, however we met them`() {
        // The holiday-acquaintance case and the relative-mid-onboarding case
        // look identical from the card, so neither is introduced. There is
        // deliberately no in-person exception.
        val outsider = contact("CruiseKid")
        val parent = cardFor("Parent", OWN_TOKEN)
        assertFalse(FriendDirectoryScope.introducible(outsider, RELAY_URL, OWN_TOKEN))
        assertEquals(emptyList<String>(), candidates(parent, listOf(outsider, parent)))
    }

    @Test
    fun `a family member joining our pass becomes eligible at that moment`() {
        // The pass-change re-fan is what replays this without user action.
        val before = contact("NotSetUpYet")
        val after = cardFor("NotSetUpYet", OWN_TOKEN)
        assertFalse(FriendDirectoryScope.introducible(before, RELAY_URL, OWN_TOKEN))
        assertTrue(FriendDirectoryScope.introducible(after, RELAY_URL, OWN_TOKEN))
    }

    @Test
    fun `with no pass of our own, nobody is introducible at all`() {
        // No family boundary is drawn, so no transitive introduction happens;
        // people scan a code or share their own friend link instead.
        val met = contact("Met")
        val other = contact("Other")
        assertEquals(emptyList<String>(), candidates(met, listOf(met, other), ownToken = null))
        assertFalse(FriendDirectoryScope.introducible(cardFor("HasPass", TESTER_TOKEN), null, null))
    }

    @Test
    fun `a pre-CP4 card carrying the member token itself is still our family`() {
        val legacy = contact("Legacy", RELAY_URL, OWN_TOKEN)
        assertTrue(FriendDirectoryScope.introducible(legacy, RELAY_URL, OWN_TOKEN))
    }

    @Test
    fun `the same family token on a different relay host is not our pass`() {
        val elsewhere = contact("Elsewhere", "https://other.example", OWN_TOKEN)
        assertFalse(FriendDirectoryScope.introducible(elsewhere, RELAY_URL, OWN_TOKEN))
    }

    @Test
    fun `a recipient is never offered themselves`() {
        val self = cardFor("Kid", OWN_TOKEN)
        assertEquals(emptyList<String>(), candidates(self, listOf(self)))
    }
}
