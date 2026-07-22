package com.cruisemesh.app.chat

import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.MeshRouterState
import com.cruisemesh.app.mesh.encodeOutboundEnvelopeFrame
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.decodeExtendedMessageBody
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.openMessage
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.util.jar.JarFile
import kotlin.io.path.absolutePathString
import kotlin.io.path.isRegularFile

/**
 * Muling hook A: when [RealMeshSender] has no direct link to the
 * recipient, it must spray the sealed envelope to every other connected link
 * instead of leaving it purely local. Exercises the real seal/store round
 * trip (same native-library harness as [ReceiptRelayRoundTripTest]) against
 * the actual [MeshRouter] singleton so the test proves the real dispatch
 * path, not a mock of it.
 */
class RealMeshSenderMulingTest {

    companion object {
        private const val HOST_CORE_LIBRARY_PROPERTY = "cruisemesh.test.hostCoreLibrary"

        init {
            configureJnaBootLibrary()
            System.setProperty(
                "uniffi.component.cruisemesh_core.libraryOverride",
                hostCoreLibrary().absolutePathString(),
            )
        }

        private fun configureJnaBootLibrary() {
            val extractedDir = Files.createTempDirectory("cruisemesh-jna")
            val dll = extractedDir.resolve("jnidispatch.dll")
            JarFile(jnaJar().toFile()).use { jar ->
                jar.getInputStream(jar.getJarEntry("com/sun/jna/win32-x86-64/jnidispatch.dll")).use { input ->
                    Files.copy(input, dll, StandardCopyOption.REPLACE_EXISTING)
                }
            }
            System.setProperty("jna.boot.library.path", extractedDir.absolutePathString())
        }

        private fun hostCoreLibrary(): Path {
            System.getProperty(HOST_CORE_LIBRARY_PROPERTY)?.let { override ->
                val overridePath = Path.of(override).normalize()
                if (overridePath.isRegularFile()) {
                    return overridePath
                }
                error("Host core library override not found at $overridePath")
            }

            val userDir = Path.of(System.getProperty("user.dir"))
            val searchRoots = linkedSetOf<Path>()
            var cursor: Path? = userDir
            while (cursor != null) {
                searchRoots.add(cursor)
                cursor.parent?.resolve("CruiseMesh")?.normalize()?.let { siblingMain ->
                    searchRoots.add(siblingMain)
                }
                cursor = cursor.parent
            }

            searchRoots.forEach { root ->
                val candidates = listOf(
                    root.resolve("target/debug/cruisemesh_core.dll"),
                    root.resolve("target/debug/libcruisemesh_core.so"),
                ).map { it.normalize() }
                val found = candidates.firstOrNull { it.isRegularFile() }
                if (found != null) {
                    return found
                }
            }

            error("Host cruisemesh_core library not found above ${userDir.toAbsolutePath()} or in a sibling CruiseMesh checkout")
        }

        private fun jnaJar(): Path {
            val cacheRoot = Path.of(System.getProperty("user.home"), ".gradle", "caches", "modules-2", "files-2.1")
            var cursor: Path? = cacheRoot.resolve("net.java.dev.jna").resolve("jna").resolve("5.14.0")
            while (cursor != null && Files.exists(cursor)) {
                Files.walk(cursor).use { paths ->
                    val found = paths
                        .filter { it.fileName.toString() == "jna-5.14.0.jar" }
                        .findFirst()
                    if (found.isPresent) {
                        return found.get()
                    }
                }
                cursor = cursor.parent
            }
            error("jna-5.14.0.jar not found under $cacheRoot")
        }
    }

    @After
    fun tearDown() {
        MeshRouter.reset()
        MeshRouter.unregisterCentral()
        MeshRouter.unregisterPeripheral()
    }

    private fun contactFor(identity: Identity, name: String) = Contact(
        userId = identity.userId,
        name = name,
        signPk = identity.signPk,
        agreePk = identity.agreePk,
        relayUrl = null,
        relayToken = null,
    )

    @Test
    fun `no direct link to the recipient sprays the sealed envelope to every other connected link`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val mule = generateIdentity()
        val bobContact = contactFor(bob, "Bob")
        val store = MessageStore.open(":memory:")
        val sender = RealMeshSender(store, alice)

        val sentFrames = mutableListOf<Pair<String, ByteArray>>()
        MeshRouter.registerCentral { address, frame -> sentFrames += address to frame }
        MeshRouter.registerPeripheral { _, _ -> }
        // A mule is connected and HELLO'd, but as someone other than Bob --
        // there is no direct link to Bob himself.
        MeshRouter.onConnected("MULE-ADDR", MeshRouterState.Transport.CENTRAL)
        MeshRouter.onHello("MULE-ADDR", mule.userId)

        assertEquals(SendResult.STORED, sender.sendText(bobContact, "hello from the pool deck"))

        val stored = store.outboundEnvelopesAfter(bobContact.userId, alice.userId, 0uL)
        assertEquals(1, stored.size)
        val expectedFrame = encodeOutboundEnvelopeFrame(stored[0])

        assertEquals(1, sentFrames.size)
        assertEquals("MULE-ADDR", sentFrames[0].first)
        assertArrayEquals(expectedFrame, sentFrames[0].second)
    }

    @Test
    fun `a direct link to the recipient is used and no mule spray happens`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val mule = generateIdentity()
        val bobContact = contactFor(bob, "Bob")
        val store = MessageStore.open(":memory:")
        val sender = RealMeshSender(store, alice)

        val sentFrames = mutableListOf<Pair<String, ByteArray>>()
        MeshRouter.registerCentral { address, frame -> sentFrames += address to frame }
        MeshRouter.registerPeripheral { _, _ -> }
        MeshRouter.onConnected("BOB-ADDR", MeshRouterState.Transport.CENTRAL)
        MeshRouter.onHello("BOB-ADDR", bob.userId)
        MeshRouter.onConnected("MULE-ADDR", MeshRouterState.Transport.CENTRAL)
        MeshRouter.onHello("MULE-ADDR", mule.userId)

        sender.sendText(bobContact, "hello directly")

        assertEquals(1, sentFrames.size)
        assertEquals("BOB-ADDR", sentFrames[0].first)
    }

    @Test
    fun `first chat message replays an unacknowledged friend card before the text`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val bobContact = contactFor(bob, "Bob")
        val store = MessageStore.open(":memory:")

        store.authorFriendRequest(
            alice,
            bobContact,
            "friend-card",
            1L,
        )

        val sentFrames = mutableListOf<ByteArray>()
        MeshRouter.registerCentral { _, frame -> sentFrames += frame }
        MeshRouter.registerPeripheral { _, _ -> }
        MeshRouter.onConnected("BOB-ADDR", MeshRouterState.Transport.CENTRAL)
        MeshRouter.onHello("BOB-ADDR", bob.userId)

        RealMeshSender(store, alice).sendText(bobContact, "hi")

        val pending = store.outboundEnvelopesAfter(bob.userId, alice.userId, 0uL).sortedBy { it.lamport }
        assertEquals(2, pending.size)
        assertEquals(2, sentFrames.size)
        assertArrayEquals(encodeOutboundEnvelopeFrame(pending[0]), sentFrames[0])
        assertArrayEquals(encodeOutboundEnvelopeFrame(pending[1]), sentFrames[1])
    }

    @Test
    fun `no connected links at all sends nothing and does not throw`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val bobContact = contactFor(bob, "Bob")
        val store = MessageStore.open(":memory:")
        val sender = RealMeshSender(store, alice)

        val sentFrames = mutableListOf<Pair<String, ByteArray>>()
        MeshRouter.registerCentral { address, frame -> sentFrames += address to frame }
        MeshRouter.registerPeripheral { _, _ -> }

        assertEquals(SendResult.STORED, sender.sendText(bobContact, "hello into the void"))

        assertEquals(0, sentFrames.size)
        assertEquals(1, store.outboundEnvelopesAfter(bobContact.userId, alice.userId, 0uL).size)
    }

    @Test
    fun `envelope failure reports not stored so the composer can retain its draft`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val invalidContact = Contact(
            userId = bob.userId,
            name = "Invalid key",
            signPk = bob.signPk,
            agreePk = byteArrayOf(1),
            relayUrl = null,
            relayToken = null,
        )
        val store = MessageStore.open(":memory:")

        val result = RealMeshSender(store, alice).sendText(invalidContact, "keep this draft")

        assertEquals(SendResult.FAILED, result)
        assertEquals(0, store.messagesForChat(invalidContact.userId).size)
        assertEquals(0, store.outboundEnvelopesAfter(invalidContact.userId, alice.userId, 0uL).size)
    }

    @Test
    fun `transport exception after persistence still reports stored and leaves a retry row`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val bobContact = contactFor(bob, "Bob")
        val store = MessageStore.open(":memory:")
        MeshRouter.registerCentral { _, _ -> error("stale transport") }
        MeshRouter.registerPeripheral { _, _ -> }
        MeshRouter.onConnected("BOB-ADDR", MeshRouterState.Transport.CENTRAL)
        MeshRouter.onHello("BOB-ADDR", bob.userId)

        val result = RealMeshSender(store, alice).sendText(bobContact, "persist before radio")

        assertEquals(SendResult.STORED, result)
        assertEquals(1, store.messagesForChat(bobContact.userId).size)
        assertEquals(1, store.outboundEnvelopesAfter(bobContact.userId, alice.userId, 0uL).size)
    }

    @Test
    fun `reply target stays encrypted and resolves from local message metadata`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val bobContact = contactFor(bob, "Bob")
        val store = MessageStore.open(":memory:")
        val sender = RealMeshSender(store, alice)

        sender.sendText(bobContact, "original")
        val originalEnvelope = store
            .outboundEnvelopesAfter(bob.userId, alice.userId, 0uL)
            .single()

        sender.sendText(bobContact, "reply", originalEnvelope.msgId)

        val messages = store.messagesForChat(bob.userId)
        val reply = messages.single { it.lamport == 2uL }
        val reference = store.messageReference(reply.chatId, reply.senderUserId, reply.lamport)
        assertNotNull(reference)
        assertArrayEquals(originalEnvelope.msgId, reference!!.replyToMsgId)

        val replyEnvelope = store
            .outboundEnvelopesAfter(bob.userId, alice.userId, 0uL)
            .single { it.lamport == 2uL }
        val opened = openMessage(bob, replyEnvelope.sealed)
        val decoded = decodeExtendedMessageBody(opened.payload)
        assertArrayEquals(originalEnvelope.msgId, decoded.replyToMsgId)

        val metadata = loadMessageReplyMetadata(store, messages) { "You" }
        val quoted = metadata[messageStableKey(reply)]?.quoted
        assertNotNull(quoted)
        assertEquals("original", quoted!!.text)
        assertEquals("You", quoted.senderLabel)
        assertEquals(1uL, quoted.target?.lamport)
    }
}
