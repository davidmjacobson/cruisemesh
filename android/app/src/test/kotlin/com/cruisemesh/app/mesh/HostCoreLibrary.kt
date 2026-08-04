package com.cruisemesh.app.mesh

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.util.jar.JarFile
import kotlin.io.path.absolutePathString
import kotlin.io.path.isRegularFile

/**
 * Points the UniFFI bindings at the *host* core library so a plain JVM unit
 * test can drive a real [uniffi.cruisemesh_core.MessageStore] -- the
 * cross-compiled `jniLibs/` builds are unloadable here. Call [load] from a
 * test class's companion `init` before touching any core type.
 *
 * Extracted verbatim from ReceiptRelayRoundTripTest, which was the only class
 * that needed it; more than one does now.
 */
object HostCoreLibrary {

    private const val HOST_CORE_LIBRARY_PROPERTY = "cruisemesh.test.hostCoreLibrary"
    private const val JNA_VERSION = "5.18.1"

    fun load() {
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
        var cursor: Path? = cacheRoot.resolve("net.java.dev.jna").resolve("jna").resolve(JNA_VERSION)
        while (cursor != null && Files.exists(cursor)) {
            Files.walk(cursor).use { paths ->
                val found = paths
                    .filter { it.fileName.toString() == "jna-$JNA_VERSION.jar" }
                    .findFirst()
                if (found.isPresent) {
                    return found.get()
                }
            }
            cursor = cursor.parent
        }
        error("jna-$JNA_VERSION.jar not found under $cacheRoot")
    }
}
