import java.io.FileInputStream
import java.util.Properties

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
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

android {
    namespace = "com.cruisemesh.app"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.cruisemesh.app"
        minSdk = 26
        // Play Console requires new releases to target API 35+.
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
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
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
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
    }
    sourceSets {
        // Kotlin bindings generated from core/ via `uniffi-bindgen` — see
        // ../../core/build-android.sh. Regenerate after changing the Rust API.
        getByName("main").kotlin.srcDirs("src/main/kotlin-gen")
    }
    testOptions {
        unitTests {
            // Production code logs routinely (android.util.Log) on paths unit
            // tests exercise directly (no Robolectric here); without this the
            // unmocked framework stub throws instead of no-op'ing, which has
            // nothing to do with what the test is actually asserting.
            isReturnDefaultValues = true
        }
    }
}

dependencies {
    implementation(libs.core.ktx)
    implementation(libs.lifecycle.runtime.ktx)
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
    debugImplementation(libs.compose.ui.tooling)
    testImplementation(libs.junit)
    testImplementation(libs.mockwebserver)
    testImplementation(libs.jna)
}
