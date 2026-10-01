import java.io.File
import java.io.FileInputStream
import java.util.Properties
import org.gradle.api.tasks.testing.Test

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.screenshot)
}

// Release signing: populated from android/app/keystore.properties (gitignored,
// see keystore.properties.example) or, for CI, the CRUISEMESH_RELEASE_* env
// vars. Neither is required for local debug work — assembleRelease falls back
// to debug signing when no upload key is configured, which is fine for local
// testing but must not be shipped to Play.
val keystoreProperties = Properties().apply {
    val propsFile = rootProject.file("app/keystore.properties")
    if (propsFile.exists()) {
        FileInputStream(propsFile).use { load(it) }
    }
}
fun signingProp(key: String, envVar: String): String? =
    keystoreProperties.getProperty(key) ?: System.getenv(envVar)

// FR15: versionCode/versionName are normally the hardcoded fallbacks below —
// bumping the epoch-timestamp versionCode by hand was the whole "artisanal
// APK builds" problem. android-release.yml passes both as -P project
// properties, derived from the triggering `android-v*` tag: versionCode is
// epoch seconds at build time (monotonic for sequential releases and above the
// epoch-style versionCodes already live on the Play track — a commit count
// would sit below them and Play would reject the upload), versionName is the
// tag text. Local builds pass neither, so `gradlew assembleDebug` and friends
// are unaffected.
val versionCodeOverride = (findProperty("versionCodeOverride") as String?)?.toIntOrNull()
val versionNameOverride = findProperty("versionNameOverride") as String?

android {
    namespace = "com.cruisemesh.app"
    compileSdk = 36

    // AGP needs an NDK to strip native libraries and to extract the symbol
    // tables that `debugSymbolLevel` (below) packages into the AAB. Without
    // one it prints "Unable to strip the following libraries" and quietly
    // produces no symbols -- a green build that ships nothing, which is how
    // Play ended up warning that this app has native code and no symbols.
    //
    // Point AGP at whatever NDK the environment already provides rather than
    // pinning `ndkVersion`: CI installs r27c via setup-ndk and exports its
    // path, while local machines have whatever Studio installed, and a pinned
    // version would break whichever side didn't match. Unset (plain local
    // builds) leaves AGP's default lookup untouched.
    // Read via `providers`, not System.getenv(): a build script's System.getenv
    // returns the Gradle *daemon's* environment, which is fixed when the daemon
    // starts, so an env var exported by the calling shell is invisible here and
    // this silently did nothing.
    providers.environmentVariable("ANDROID_NDK_HOME").orNull
        ?.takeIf { it.isNotBlank() }
        ?.let { ndkPath = it }

    defaultConfig {
        applicationId = "com.cruisemesh.app"
        // minSdk 31 (S): mesh needs the API-31 BLUETOOTH_SCAN/ADVERTISE/CONNECT
        // permission trio (see AndroidManifest.xml) with no legacy fallback —
        // BLE mesh silently fails or crashes with SecurityException below API 31.
        minSdk = 31
        // Play Console requires new apps/updates to target API 36+ from
        // 2026-08-31 (API 35 was the floor before that).
        targetSdk = 36
        versionCode = versionCodeOverride ?: 1784406677
        versionName = versionNameOverride ?: "1.0.0"
        ndk {
            // Keep dependency-provided native libraries on the exact ABI set
            // built by core/build-android.sh. JNA also publishes legacy ABIs;
            // packaging those without libcruisemesh_core.so would create
            // device splits that fail at startup.
            abiFilters += setOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        testInstrumentationRunnerArguments["clearPackageData"] = "true"
    }

    signingConfigs {
        create("release") {
            val storeFilePath = signingProp("storeFile", "CRUISEMESH_RELEASE_STORE_FILE")
            if (storeFilePath != null) {
                storeFile = rootProject.file("app/$storeFilePath")
                storePassword = signingProp("storePassword", "CRUISEMESH_RELEASE_STORE_PASSWORD")
                keyAlias = signingProp("keyAlias", "CRUISEMESH_RELEASE_KEY_ALIAS")
                keyPassword = signingProp("keyPassword", "CRUISEMESH_RELEASE_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        debug {
            // Distinct package so a debug build can sit NEXT TO the Play
            // install on the same phone instead of demanding an uninstall
            // (debug/release signatures can never upgrade over each other).
            // Everything runtime derives the package dynamically
            // (${applicationId} in the manifest, context.packageName in
            // code), so this is safe; adb tooling must target
            // com.cruisemesh.app.debug and the fully qualified activity
            // class (the Kotlin package does NOT gain the suffix).
            applicationIdSuffix = ".debug"
            versionNameSuffix = "-debug"
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            ndk {
                // Nearly all of this app's logic is Rust behind JNI, so an
                // unsymbolicated native crash is a bare address and nothing
                // else -- exactly the crashes we can least afford to lose.
                // Play warns "you've not uploaded debug symbols" without this.
                //
                // SYMBOL_TABLE, not FULL: cargo's release profile emits no
                // DWARF (no `debug = true`), so the .so files carry a symbol
                // table and no .debug_* sections. FULL would package the same
                // information under a slower build. If line numbers are ever
                // wanted, that starts with debug info in the Rust profile.
                //
                // AAB-only, and Play strips symbols before delivery, so this
                // costs the download nothing.
                debugSymbolLevel = "SYMBOL_TABLE"
            }
            val releaseSigning = signingConfigs.getByName("release")
            signingConfig = if (releaseSigning.storeFile != null) {
                releaseSigning
            } else {
                logger.warn("No release keystore configured (app/keystore.properties) — signing assembleRelease with the debug key. Do NOT upload this build to Play.")
                signingConfigs.getByName("debug")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        compose = true
    }
    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
        jniLibs {
            // Sync-check marker (see verifyNativeBindingsSync below) — lives at
            // the jniLibs root, not inside an ABI dir, so AGP wouldn't normally
            // package it, but exclude it explicitly to be safe.
            excludes += "/.cruisemesh-native-stamp"
        }
    }
    sourceSets {
        // Kotlin bindings generated from core/ via `uniffi-bindgen` — see
        // ../../core/build-android.sh. Regenerate after changing the Rust API.
        getByName("main").kotlin.srcDirs("src/main/kotlin-gen")
    }
    testOptions {
        animationsDisabled = true
        execution = "ANDROID_TEST_ORCHESTRATOR"
        unitTests {
            // Production code logs routinely (android.util.Log) on paths unit
            // tests exercise directly (no Robolectric here); without this the
            // unmocked framework stub throws instead of no-op'ing, which has
            // nothing to do with what the test is actually asserting.
            isReturnDefaultValues = true
            isIncludeAndroidResources = true
        }
        managedDevices {
            localDevices {
                create("pixel6Api36") {
                    device = "Pixel 6"
                    apiLevel = 36
                    systemImageSource = "aosp"
                }
                create("pixel2Api31") {
                    device = "Pixel 2"
                    apiLevel = 31
                    systemImageSource = "aosp"
                }
            }
        }
    }
    experimentalProperties["android.experimental.enableScreenshotTest"] = true
}

dependencies {
    implementation(libs.core.ktx)
    implementation(libs.lifecycle.runtime.ktx)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.activity.compose)
    implementation(libs.navigation.compose)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.zxing.core)
    implementation(libs.jna) { artifact { type = "aar" } }
    implementation(libs.camerax.core)
    implementation(libs.camerax.camera2)
    implementation(libs.camerax.lifecycle)
    implementation(libs.camerax.view)
    implementation(libs.gson)
    implementation(libs.exifinterface)
    // Relay WS push (RelayPushClient): java.net has no WebSocket client, and
    // hand-rolling the handshake/framing/TLS is a lot of surface for what is
    // strictly a latency optimization over the existing poll/fetch path.
    // okhttp's version is already pinned in the catalog (mockwebserver pulls
    // it in transitively for tests); this just also exposes it to main code.
    implementation(libs.okhttp)
    debugImplementation(libs.compose.ui.tooling)
    debugImplementation(libs.compose.ui.test.manifest)
    testImplementation(libs.junit)
    testImplementation(libs.mockwebserver)
    testImplementation(libs.jna)
    testImplementation(platform(libs.compose.bom))
    testImplementation(libs.compose.ui.test.junit4)
    testImplementation(libs.androidx.test.core)
    testImplementation(libs.androidx.test.junit)
    testImplementation(libs.navigation.testing)
    testImplementation(libs.robolectric)

    androidTestImplementation(platform(libs.compose.bom))
    androidTestImplementation(libs.compose.ui.test.junit4)
    androidTestImplementation(libs.androidx.test.core)
    androidTestImplementation(libs.androidx.test.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.rules)
    androidTestUtil(libs.androidx.test.orchestrator)

    screenshotTestImplementation(libs.compose.ui.tooling)
    // @PreviewTest — without it the screenshot plugin collects nothing. compileOnly
    // on purpose: the annotation is only needed to compile the previews, and the
    // plugin's engine already carries its own matching copy at test runtime. See
    // the catalog comment on screenshotValidationApi for the version split.
    screenshotTestCompileOnly(libs.screenshot.validation.api)
}

// Pure JVM tests now exercise portable logic through UniFFI. Point JNA at the
// host library produced by the documented pre-test `cargo build` step.
tasks.withType<Test>().configureEach {
    val libraryName = when {
        System.getProperty("os.name").startsWith("Windows", ignoreCase = true) -> "cruisemesh_core.dll"
        System.getProperty("os.name").startsWith("Mac", ignoreCase = true) -> "libcruisemesh_core.dylib"
        else -> "libcruisemesh_core.so"
    }
    val hostLibrary = rootProject.projectDir.parentFile.resolve("target/debug/$libraryName")
    if (hostLibrary.isFile) {
        systemProperty("uniffi.component.cruisemesh_core.libraryOverride", hostLibrary.absolutePath)
    }
}

// Fails the build fast (instead of at app launch with an UnsatisfiedLinkError/
// UniFFI checksum mismatch) if kotlin-gen/ and jniLibs/ weren't produced by the
// same core/build-android.sh run — each run stamps both dirs with a matching
// .cruisemesh-native-stamp value; see that script.
val verifyNativeBindingsSync = tasks.register("verifyNativeBindingsSync") {
    val kotlinGenDir = layout.projectDirectory.dir("src/main/kotlin-gen")
    val jniLibsDir = layout.projectDirectory.dir("src/main/jniLibs")
    val kotlinStamp = kotlinGenDir.file(".cruisemesh-native-stamp")
    val jniStamp = jniLibsDir.file(".cruisemesh-native-stamp")

    // Not real task inputs/outputs (this is a fast sanity check, not a
    // cacheable transform) — always re-run so a stale UP-TO-DATE never masks
    // a drift introduced by a later build-android.sh run.
    outputs.upToDateWhen { false }

    doFirst {
        val fixIt = "run core/build-android.sh to regenerate both together"

        val missingDirs = listOf(kotlinGenDir.asFile, jniLibsDir.asFile).filterNot { it.isDirectory }
        if (missingDirs.isNotEmpty()) {
            throw GradleException(
                "Native libs and UniFFI bindings are out of sync (or missing) — $fixIt.\n" +
                    "Missing director" + (if (missingDirs.size == 1) "y" else "ies") + ": " +
                    missingDirs.joinToString { it.path }
            )
        }

        val kotlinStampFile = kotlinStamp.asFile
        val jniStampFile = jniStamp.asFile
        val missingStamps = listOf(kotlinStampFile, jniStampFile).filterNot { it.isFile }
        if (missingStamps.isNotEmpty()) {
            throw GradleException(
                "Native libs and UniFFI bindings are out of sync (or missing) — $fixIt.\n" +
                    "Missing stamp file(s): " + missingStamps.joinToString { it.path }
            )
        }

        val kotlinValue = kotlinStampFile.readText().trim()
        val jniValue = jniStampFile.readText().trim()
        if (kotlinValue.isEmpty() || jniValue.isEmpty() || kotlinValue != jniValue) {
            throw GradleException(
                "Native libs and UniFFI bindings are out of sync (or missing) — $fixIt.\n" +
                    "Stamp mismatch: kotlin-gen=$kotlinValue jniLibs=$jniValue"
            )
        }
    }
}

// A screenshot gate that renders nothing still reports success. That is not
// hypothetical here: screenshot plugin alpha10 narrowed collection to previews
// annotated @PreviewTest, a dependency bump moved the pin from alpha09 to
// alpha16, and validateDebugScreenshotTest went on passing while rendering zero
// of the 14 previews -- `tests="0"` in its own result XML, with 14 reference
// images in the tree comparing against nothing. Nothing failed, because nothing
// ran, and only reading the XML by hand showed it.
//
// So assert on the count, not the exit status. Three numbers have to agree:
// previews declared in src/screenshotTest, reference images committed under
// src/screenshotTestDebug/reference, and testcases in the run's result XML.
// Zero collected is the loud case; one preview quietly losing its annotation, or
// a reference image left behind comparing against nothing, are the likelier
// ones. Missing references already fail validation itself; orphaned ones do not,
// which is how a whole directory of them survived the alpha09-to-alpha16 move.
val screenshotPreviewSources = layout.projectDirectory.dir("src/screenshotTest")
val screenshotReferenceImages = layout.projectDirectory.dir("src/screenshotTestDebug/reference")
val screenshotTestResults = layout.buildDirectory.dir("test-results/validateDebugScreenshotTest")

val verifyScreenshotPreviewsCollected = tasks.register("verifyScreenshotPreviewsCollected") {
    group = "verification"
    description = "Fails if validateDebugScreenshotTest did not compare every declared preview against its reference image."

    val sourcesDir = screenshotPreviewSources.asFile
    val referenceDir = screenshotReferenceImages.asFile
    val resultsDir = screenshotTestResults

    // Same reasoning as verifyNativeBindingsSync: a cheap consistency check, not
    // a cacheable transform. An UP-TO-DATE here would be a gate that stopped
    // checking, which is the exact failure this task exists to prevent.
    outputs.upToDateWhen { false }

    doLast {
        // Annotations only ever start a line in these sources; skipping comment
        // lines keeps prose that mentions the annotations from being counted.
        val previewLine = Regex("""^\s*@Preview\s*(\(|$)""")
        val declared = sourcesDir.walkTopDown()
            .filter { it.isFile && it.extension == "kt" }
            .sumOf { source ->
                source.readLines().count { line ->
                    val trimmed = line.trim()
                    !trimmed.startsWith("//") && !trimmed.startsWith("*") && previewLine.containsMatchIn(line)
                }
            }

        val resultFiles = resultsDir.get().asFile
            .listFiles { file: File -> file.isFile && file.name.endsWith(".xml") }
            ?.sortedBy { it.name }
            .orEmpty()
        if (resultFiles.isEmpty()) {
            throw GradleException(
                "Screenshot gate produced no result XML in ${resultsDir.get().asFile.path} — " +
                    "run :app:validateDebugScreenshotTest, which this task verifies."
            )
        }

        val testsAttribute = Regex("""<testsuite\b[^>]*\btests="(\d+)"""")
        val collected = resultFiles.sumOf { file ->
            val matches = testsAttribute.findAll(file.readText()).toList()
            if (matches.isEmpty()) {
                throw GradleException(
                    "Screenshot gate result XML has no testsuite count: ${file.path}"
                )
            }
            matches.sumOf { it.groupValues[1].toInt() }
        }

        if (collected != declared) {
            throw GradleException(
                "Screenshot gate validated $collected of $declared previews declared in " +
                    "${sourcesDir.path}.\n" +
                    "Every preview needs @PreviewTest as well as @Preview — without it the " +
                    "screenshot plugin does not collect it, and the gate passes without " +
                    "rendering it. Counted from ${resultFiles.joinToString { it.name }}."
            )
        }

        val references = referenceDir.walkTopDown().count { it.isFile && it.extension == "png" }
        if (references != collected) {
            throw GradleException(
                "Screenshot gate validated $collected previews but ${referenceDir.path} holds " +
                    "$references reference image(s).\n" +
                    "A reference nothing compares against is worse than no reference: delete " +
                    "the leftovers, or regenerate with :app:updateDebugScreenshotTest if the " +
                    "plugin changed where they live."
            )
        }

        logger.lifecycle("Screenshot gate compared $collected previews against $references reference images.")
    }
}

// finalizedBy, not dependsOn: CI and AGENTS.md both invoke
// :app:validateDebugScreenshotTest directly, so the check has to ride that task
// rather than sit behind a name someone has to remember to type.
tasks.matching { it.name.matches(Regex("validate\\w*ScreenshotTest")) }.configureEach {
    finalizedBy(verifyScreenshotPreviewsCollected)
}

// Gate the check on the tasks that actually package jniLibs into an APK/AAB,
// not preBuild: JVM unit tests load the host cdylib via the libraryOverride
// above and never touch jniLibs/, and CI's android-units job (rust.yml)
// deliberately provisions only kotlin-gen + the host library on a fresh
// checkout where jniLibs/ has never existed.
tasks.matching { it.name.matches(Regex("merge\\w*JniLibFolders")) }.configureEach {
    dependsOn(verifyNativeBindingsSync)
}
