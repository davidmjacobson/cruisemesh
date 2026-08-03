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

    private fun candidates(recipient: Contact, contacts: List<Contact>) =
        FriendDirectoryScope.candidatesFor(recipient, contacts, RELAY_URL, OWN_TOKEN)
            .map { it.name }

    @Test
    fun `a contact on another pass is never offered and never receives`() {
        val family = cardFor("Sibling", OWN_TOKEN)
        val tester = cardFor("Tester", TESTER_TOKEN)
        val contacts = listOf(family, tester)

        // The reported symptom: a tester-pass person offered inside a family.
        assertEquals(listOf("Sibling"), candidates(recipient = cardFor("Kid", OWN_TOKEN), contacts))
        // ...and the same leak outbound, which would hand a family's names to
        // the tester fleet.
        assertEquals(emptyList<String>(), candidates(recipient = tester, contacts))
    }

    @Test
    fun `family introductions still work, which is the whole point of the feature`() {
        val contacts = listOf(
            cardFor("Parent", OWN_TOKEN),
            cardFor("Kid1", OWN_TOKEN),
            cardFor("Kid2", OWN_TOKEN),
        )
        assertEquals(
            listOf("Parent", "Kid2"),
            candidates(recipient = contacts[1], contacts = contacts),
        )
    }

    @Test
    fun `a family member who has not set a pass up yet stays eligible`() {
        // Their card carries no relay fields, so our sends to them already
        // land in our own mailbox. Excluding them would break introductions
        // for exactly the half-onboarded family the feature helps most.
        val noPass = contact("NotSetUpYet")
        val contacts = listOf(noPass, cardFor("Parent", OWN_TOKEN))
        assertTrue(FriendDirectoryScope.sharesOwnPass(noPass, RELAY_URL, OWN_TOKEN))
        assertEquals(listOf("NotSetUpYet"), candidates(recipient = contacts[1], contacts))
    }

    @Test
    fun `a pre-CP4 card carrying the member token itself is still our family`() {
        val legacy = contact("Legacy", RELAY_URL, OWN_TOKEN)
        assertTrue(FriendDirectoryScope.sharesOwnPass(legacy, RELAY_URL, OWN_TOKEN))
    }

    @Test
    fun `the same family token on a different relay host is not our pass`() {
        val elsewhere = contact("Elsewhere", "https://other.example", OWN_TOKEN)
        assertFalse(FriendDirectoryScope.sharesOwnPass(elsewhere, RELAY_URL, OWN_TOKEN))
    }

    @Test
    fun `a recipient is never offered themselves`() {
        val self = cardFor("Kid", OWN_TOKEN)
        assertEquals(emptyList<String>(), candidates(recipient = self, contacts = listOf(self)))
    }

    @Test
    fun `with no pass of our own nobody is excluded`() {
        // Nothing to compare against; silently emptying every snapshot would
        // switch the feature off for anyone who has not bought a pass.
        val tester = cardFor("Tester", TESTER_TOKEN)
        val other = cardFor("Other", TESTER_TOKEN)
        assertEquals(
            listOf("Other"),
            FriendDirectoryScope.candidatesFor(tester, listOf(tester, other), null, null)
                .map { it.name },
        )
    }
}
