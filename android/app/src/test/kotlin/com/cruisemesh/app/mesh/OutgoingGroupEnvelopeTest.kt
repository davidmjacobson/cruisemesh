package com.cruisemesh.app.mesh

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.decodeExtendedMessageBody
import uniffi.cruisemesh_core.decodeMessageBody
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.openGroupMessage
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.util.jar.JarFile
import kotlin.io.path.absolutePathString
import kotlin.io.path.isRegularFile

/**
 * Headless check that a group-authored outbound envelope seals with the
 * shared group key and re-opens to the same plaintext body.
 */
class OutgoingGroupEnvelopeTest {

    companion object {
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
            val userDir = Path.of(System.getProperty("user.dir"))
            var cursor: Path? = userDir
            while (cursor != null) {
                val candidates = listOf(
                    cursor.resolve("target/debug/cruisemesh_core.dll"),
                    cursor.resolve("target/debug/libcruisemesh_core.so"),
                ).map { it.normalize() }
                val found = candidates.firstOrNull { it.isRegularFile() }
                if (found != null) {
                    return found
                }
                cursor = cursor.parent
            }
            error("Host cruisemesh_core library not found above ${userDir.toAbsolutePath()}")
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
    fun groupEnvelopeRoundTripsThroughSharedKey() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val group = createGroup(
            "Bridge Crew",
            listOf(alice.userId, bob.userId),
        )
        val message = StoredMessage(
            chatId = group.id,
            senderUserId = alice.userId,
            lamport = 1uL,
            timestamp = 1_700_000_000_000L,
            kind = 1u.toUByte(),
            payload = "dinner at 7".toByteArray(Charsets.UTF_8),
        )

        val outbound = buildOutboundGroupEnvelope(alice, group, message)
        assertNotNull(outbound)
        assertArrayEquals(group.id, outbound!!.recipientUserId)
        assertArrayEquals(group.id, outbound.chatId)
        assertEquals(1u.toUByte(), outbound.kind)

        val opened = openGroupMessage(group, outbound.sealed)
        assertArrayEquals(alice.userId, opened.senderUserId)
        val body = decodeMessageBody(opened.payload)
        assertArrayEquals(group.id, body.chatId)
        assertEquals("dinner at 7", body.content.toString(Charsets.UTF_8))
    }

    @Test
    fun groupReplyReferenceRoundTripsInsideSharedCiphertext() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val group = createGroup("Bridge Crew", listOf(alice.userId, bob.userId))
        val replyToMsgId = ByteArray(16) { (it + 1).toByte() }
        val message = StoredMessage(
            chatId = group.id,
            senderUserId = alice.userId,
            lamport = 2uL,
            timestamp = 1_700_000_001_000L,
            kind = 1u,
            payload = "sounds good".toByteArray(),
        )

        val outbound = buildOutboundGroupEnvelope(alice, group, message, replyToMsgId)
        assertNotNull(outbound)

        val opened = openGroupMessage(group, outbound!!.sealed)
        val body = decodeExtendedMessageBody(opened.payload)
        assertEquals("sounds good", body.content.toString(Charsets.UTF_8))
        assertArrayEquals(replyToMsgId, body.replyToMsgId)
    }
}
