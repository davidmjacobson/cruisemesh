package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreCarriedOfferGate
import uniffi.cruisemesh_core.CoreMeshRouterState
import uniffi.cruisemesh_core.CoreSprayPolicy
import uniffi.cruisemesh_core.CoreSprayTrigger
import uniffi.cruisemesh_core.CoreTransport
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.SeenIds
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.parseFrame
import uniffi.cruisemesh_core.sealMessage
import java.io.File

/**
 * The encounter path running on the core planner.
 *
 * The claim under test is the seam, not the planner: `mesh_meet.rs` owns the
 * ordering, the budgets and the carry lifecycle and has its own Rust tests for
 * all of it. What can only be proven here is that this shell hands the planner
 * a real encounter and then actually puts the frames it returns on the
 * transport, in the order it returned them -- and that with the flag at its
 * shipped default the planner is not reached at all.
 */
class CoreMeetEngineTest {

    private companion object {
        const val ADDRESS = "AA:BB:CC:DD:EE:FF"
    }

    private class FakePrefs : SharedPreferences {
        private val map = mutableMapOf<String, Any?>()

        override fun getAll(): MutableMap<String, *> = map
        override fun getString(key: String?, def: String?) = map[key] as? String ?: def
        @Suppress("UNCHECKED_CAST")
        override fun getStringSet(key: String?, def: MutableSet<String>?) =
            map[key] as? MutableSet<String> ?: def
        override fun getInt(key: String?, def: Int) = map[key] as? Int ?: def
        override fun getLong(key: String?, def: Long) = map[key] as? Long ?: def
        override fun getFloat(key: String?, def: Float) = map[key] as? Float ?: def
        override fun getBoolean(key: String?, def: Boolean) = map[key] as? Boolean ?: def
        override fun contains(key: String?) = map.containsKey(key)
        override fun edit(): SharedPreferences.Editor = Editor()
        override fun registerOnSharedPreferenceChangeListener(
            l: SharedPreferences.OnSharedPreferenceChangeListener?
        ) {}
        override fun unregisterOnSharedPreferenceChangeListener(
            l: SharedPreferences.OnSharedPreferenceChangeListener?
        ) {}

        inner class Editor : SharedPreferences.Editor {
            override fun putString(key: String?, value: String?) = apply { map[key!!] = value }
            override fun putStringSet(key: String?, value: MutableSet<String>?) =
                apply { map[key!!] = value }
            override fun putInt(key: String?, value: Int) = apply { map[key!!] = value }
            override fun putLong(key: String?, value: Long) = apply { map[key!!] = value }
            override fun putFloat(key: String?, value: Float) = apply { map[key!!] = value }
            override fun putBoolean(key: String?, value: Boolean) = apply { map[key!!] = value }
            override fun remove(key: String?) = apply { map.remove(key) }
            override fun clear() = apply { map.clear() }
            override fun commit() = true
            override fun apply() {}
        }
    }

    private class FakeContext : ContextWrapper(null) {
        private val prefs = mutableMapOf<String, FakePrefs>()
        private val files = File.createTempFile("coremeet", null).parentFile!!

        override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
            prefs.getOrPut(name ?: "") { FakePrefs() }
        override fun getApplicationContext(): Context = this
        override fun getFilesDir(): File = files
    }

    /** Every frame the adapter asked the shell to put on a link, in order. */
    private class RecordingLinks : CoreMeetAdapter.Links {
        val sent = mutableListOf<Pair<String, ByteArray>>()

        override fun send(address: String, frame: ByteArray) {
            sent += address to frame
        }
    }

    /**
     * One encounter, wired exactly as `MeshService` wires it: the process-wide
     * route state, spray policy and offer gate passed in rather than rebuilt,
     * because the planner records its windows and cursors on them.
     */
    private class Rig(val store: MessageStore, val me: Identity, val peer: Identity) {
        val router = CoreMeshRouterState()
        val spray = CoreSprayPolicy()
        val offers = CoreCarriedOfferGate()
        val links = RecordingLinks()
        var now = 1_700_000_000_000L

        init {
            router.setLocalUserId(me.userId)
            router.onConnected(ADDRESS, CoreTransport.CENTRAL)
            check(router.onHello(ADDRESS, peer.userId))
        }

        val adapter get() = CoreMeetAdapter(
            store = store,
            router = router,
            spray = spray,
            offers = offers,
            links = links,
            now = { now },
            sprayNow = { now },
        )
    }

    private fun contactOf(identity: Identity, name: String) = Contact(
        userId = identity.userId,
        name = name,
        signPk = identity.signPk,
        agreePk = identity.agreePk,
        relayUrl = null,
        relayToken = null,
    )

    /**
     * A sealed envelope from a stranger to [recipient], carried into [store]
     * through the ordinary inbound path so the carry row is classified exactly
     * as a real mule copy would be.
     */
    private fun carryOneFor(store: MessageStore, me: Identity, recipient: Identity, now: Long): ByteArray {
        val sender = generateIdentity()
        val envelope = Frame.Envelope(
            msgId = generateMsgId(),
            hopTtl = 7u,
            expiry = now + 600_000,
            recipientHint = computeRecipientHint(recipient.userId, now),
            sealed = sealMessage(
                sender,
                recipient.agreePk,
                encodeMessageBody(
                    MessageBody(
                        kind = KIND_TEXT,
                        chatId = sender.userId,
                        lamport = 1u,
                        timestamp = now,
                        content = "for you".toByteArray(),
                    ),
                ),
            ),
        )
        val intents = object : CoreInboundAdapter.Intents {
            override fun flood(sourceAddress: String?, frame: ByteArray) {}
            override fun deliver(
                sourceAddress: String?,
                hopTtl: UByte,
                msgId: ByteArray,
                senderUserId: ByteArray,
                groupId: ByteArray?,
                payload: ByteArray,
                identity: Identity,
            ): Boolean = true
            override fun onFamilyCarry() {}
        }
        CoreInboundAdapter(store, SeenIds(), intents) { now }
            .process("11:22:33:44:55:66", envelope, me)
        return envelope.msgId
    }

    // ---- The flag itself ------------------------------------------------

    @Test
    fun theShippedDefaultIsTheLegacySequencer() {
        val context = FakeContext()
        assertEquals(MeetEngine.LEGACY, MeetEngineSettings.meetEngine(context))

        MeetEngineSettings.setMeetEngine(context, MeetEngine.CORE)
        assertEquals(MeetEngine.CORE, MeetEngineSettings.meetEngine(context))

        MeetEngineSettings.setMeetEngine(context, MeetEngine.LEGACY)
        assertEquals(MeetEngine.LEGACY, MeetEngineSettings.meetEngine(context))
    }

    /**
     * The branch point, in the shape every call site in `MeshService` uses it:
     * the planner is reached only on the CORE selection, so a device on the
     * shipped default runs the sequencing it ran before this package and the
     * adapter is never asked for a frame.
     */
    @Test
    fun aLegacyFlaggedEncounterNeverReachesThePlanner() {
        val context = FakeContext()
        val me = generateIdentity()
        val peer = generateIdentity()
        val store = MessageStore.open(":memory:")
        store.upsertImportedContact(contactOf(peer, "Peer"))
        val rig = Rig(store, me, peer)
        carryOneFor(store, me, peer, rig.now)
        assertEquals("the carry that a CORE encounter would offer", 1uL, store.carriedLen())

        if (MeetEngineSettings.meetEngine(context) == MeetEngine.CORE) {
            rig.adapter.encounter(ADDRESS, me.userId, peer.userId, CoreSprayTrigger.FIRST_CONTACT)
        }

        assertTrue("the legacy branch put nothing on the link", rig.links.sent.isEmpty())
        assertEquals("and touched no carry row", 1uL, store.carriedLen())
    }

    // ---- The CORE branch ------------------------------------------------

    /**
     * A first-contact encounter under CORE: the planner's digest goes out
     * first and the hint-matched carry follows it. The ordering is the
     * load-bearing part -- core's exchange window opens when the digest is
     * enqueued, and a multi-KB drain ahead of it on a BLE link would hold it in
     * the FIFO past that window.
     */
    @Test
    fun aCoreFlaggedEncounterSendsTheDigestThenTheTargetedCarry() {
        val context = FakeContext()
        MeetEngineSettings.setMeetEngine(context, MeetEngine.CORE)
        val me = generateIdentity()
        val peer = generateIdentity()
        val store = MessageStore.open(":memory:")
        store.upsertImportedContact(contactOf(peer, "Peer"))
        val rig = Rig(store, me, peer)
        val carriedMsgId = carryOneFor(store, me, peer, rig.now)

        assertEquals(MeetEngine.CORE, MeetEngineSettings.meetEngine(context))
        val work = rig.adapter.encounter(
            ADDRESS,
            me.userId,
            peer.userId,
            CoreSprayTrigger.FIRST_CONTACT,
        )

        requireNotNull(work)
        assertEquals("one 1:1 digest was owed on a link that never ran one", 1u, work.digestsSent)
        assertEquals("the carry hint-matched this peer", 1u, work.targetedSent)

        val frames = rig.links.sent.map { (address, frame) ->
            assertEquals(ADDRESS, address)
            parseFrame(frame)
        }
        assertEquals(2, frames.size)
        assertTrue("the digest is first", frames[0] is Frame.Digest)
        val carried = frames[1] as Frame.Envelope
        assertTrue(
            "the drained frame is the carried envelope",
            carried.msgId.contentEquals(carriedMsgId),
        )

        // CARRY-01/DTN D2: dispatch is not proof of receipt. The row survives
        // being offered, and only the peer's digest can retire it.
        assertEquals("the carry row survives dispatch", 1uL, store.carriedLen())
        assertEquals(0u, work.confirmedRemoved)
    }

    /**
     * A peer digest answered under CORE, on an authenticated link: the ids the
     * peer advertised retire the copy we were carrying for them (CARRY-02),
     * and nothing is re-offered -- answering a digest must never provoke one
     * back, or two converged phones ping-pong for as long as they stay in
     * range.
     */
    @Test
    fun anAuthenticatedPeerDigestRetiresTheCarryAndProvokesNoDigestBack() {
        val me = generateIdentity()
        val peer = generateIdentity()
        val store = MessageStore.open(":memory:")
        store.upsertImportedContact(contactOf(peer, "Peer"))
        val rig = Rig(store, me, peer)
        val carriedMsgId = carryOneFor(store, me, peer, rig.now)

        val work = rig.adapter.encounter(
            ADDRESS,
            me.userId,
            peer.userId,
            CoreSprayTrigger.PEER_DIGEST,
            peerKnownMsgIds = listOf(carriedMsgId),
            peerAuthenticated = true,
        )

        requireNotNull(work)
        assertEquals("answering a digest owes no digest back", 0u, work.digestsSent)
        assertEquals("the peer already holds it", 0u, work.targetedSent)
        assertEquals("proof of receipt retired the carry", 1u, work.confirmedRemoved)
        assertEquals(0uL, store.carriedLen())
        assertTrue("nothing to send", rig.links.sent.isEmpty())
    }

    /**
     * The same digest over an unauthenticated link. CARRY-02: a bare BLE claim
     * still suppresses the re-offer this encounter would have made, but it may
     * never delete the durable copy.
     */
    @Test
    fun anUnauthenticatedPeerDigestSuppressesTheOfferButKeepsTheCarry() {
        val me = generateIdentity()
        val peer = generateIdentity()
        val store = MessageStore.open(":memory:")
        store.upsertImportedContact(contactOf(peer, "Peer"))
        val rig = Rig(store, me, peer)
        val carriedMsgId = carryOneFor(store, me, peer, rig.now)

        val work = rig.adapter.encounter(
            ADDRESS,
            me.userId,
            peer.userId,
            CoreSprayTrigger.PEER_DIGEST,
            peerKnownMsgIds = listOf(carriedMsgId),
            peerAuthenticated = false,
        )

        requireNotNull(work)
        assertEquals("an unauthenticated claim never deletes", 0u, work.confirmedRemoved)
        assertEquals("but it is still honoured as an exclusion", 1u, work.skippedKnown)
        assertEquals("the durable copy stays", 1uL, store.carriedLen())
        assertTrue(rig.links.sent.isEmpty())
    }

    /**
     * The planner records its progress on the objects this shell already
     * holds, not on ones it built for the call. If it did not, a second
     * encounter on the same link would owe a second digest immediately and the
     * re-digest window would never mean anything.
     */
    @Test
    fun asecondEncounterOnTheSameLinkIsInsideTheReDigestWindow() {
        val me = generateIdentity()
        val peer = generateIdentity()
        val store = MessageStore.open(":memory:")
        store.upsertImportedContact(contactOf(peer, "Peer"))
        val rig = Rig(store, me, peer)

        val first = rig.adapter.encounter(ADDRESS, me.userId, peer.userId, CoreSprayTrigger.FIRST_CONTACT)
        assertEquals(1u, requireNotNull(first).digestsSent)

        rig.now += 1_000L
        val second = rig.adapter.encounter(ADDRESS, me.userId, peer.userId, CoreSprayTrigger.RECONNECT)
        assertEquals(
            "the window the first encounter armed is still shut",
            0u,
            requireNotNull(second).digestsSent,
        )
        assertFalse("and the shell sent nothing a second time", rig.links.sent.size > 1)
    }
}
