package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.decodeMessageBody
import uniffi.cruisemesh_core.decodeReceiptContent
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.openMessage
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.util.jar.JarFile
import kotlin.io.path.absolutePathString
import kotlin.io.path.isRegularFile

class ReceiptRelayRoundTripTest {

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

    @Test
    fun `relay receipt round trip advances sender delivered and read watermarks without persisting receipt messages`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val aliceContact = contactFor(bob, "Bob")
        val bobContact = contactFor(alice, "Alice")
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        aliceStore.upsertContact(aliceContact)
        bobStore.upsertContact(bobContact)

        val textEnvelope = aliceStore.authorPairwiseMessage(
            alice,
            aliceContact,
            1u,
            "relay-text".toByteArray(),
            null,
            1_700_000_000_000L,
        ).envelope

        val openedText = openMessage(bob, textEnvelope.sealed)
        val decodedText = decodeMessageBody(openedText.payload)
        bobStore.insertMessage(
            StoredMessage(
                chatId = openedText.senderUserId,
                senderUserId = openedText.senderUserId,
                lamport = decodedText.lamport,
                timestamp = decodedText.timestamp,
                kind = decodedText.kind,
                payload = decodedText.content,
            ),
        )

        val through = bobStore.highestContiguousLamport(alice.userId, alice.userId)
        assertEquals(1uL, through)
        bobStore.recordOutgoingReceipt(alice.userId, alice.userId, 1u, through)
        bobStore.recordOutgoingReceipt(alice.userId, alice.userId, 2u, through)

        val deliveredEnvelope = bobStore.ensureAuthoredReceipt(
            bob,
            bobContact,
            alice.userId,
            1u,
            through,
            1_700_000_000_100L,
        ).envelope
        val readEnvelope = bobStore.ensureAuthoredReceipt(
            bob,
            bobContact,
            alice.userId,
            2u,
            through,
            1_700_000_000_200L,
        ).envelope

        assertEquals(
            1,
            bobStore.messagesForChat(alice.userId).size,
        )
        val actualEnvelopes = bobStore.pendingRelayOutgoingReceiptEnvelopes(10uL, 1_700_000_000_300L)
        assertEquals(2, actualEnvelopes.size)
        assertEquals(deliveredEnvelope.toString(), actualEnvelopes[0].toString())
        assertEquals(readEnvelope.toString(), actualEnvelopes[1].toString())

        ingestReceiptEnvelope(aliceStore, alice, deliveredEnvelope)
        ingestReceiptEnvelope(aliceStore, alice, readEnvelope)

        assertEquals(1uL, aliceStore.receiptThrough(bob.userId, alice.userId, 1u))
        assertEquals(1uL, aliceStore.receiptThrough(bob.userId, alice.userId, 2u))
        assertEquals(1, aliceStore.messagesForChat(bob.userId).size)
    }

    private fun ingestReceiptEnvelope(
        store: MessageStore,
        recipient: Identity,
        envelope: uniffi.cruisemesh_core.OutgoingReceiptEnvelope,
    ) {
        val opened = openMessage(recipient, envelope.sealed)
        val body = decodeMessageBody(opened.payload)
        val receipt = decodeReceiptContent(body.content)
        store.recordReceipt(
            chatId = opened.senderUserId,
            senderUserId = recipient.userId,
            receiptType = receipt.receiptType,
            throughLamport = receipt.lamport,
        )
    }

    private fun contactFor(identity: Identity, name: String) = Contact(
        userId = identity.userId,
        name = name,
        signPk = identity.signPk,
        agreePk = identity.agreePk,
        relayUrl = "https://relay.example.test",
        relayToken = "token-$name",
    )
}
