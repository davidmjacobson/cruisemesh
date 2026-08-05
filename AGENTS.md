# Agent Notes

Build and bindgen recipes that don't fit in [README.md](README.md), including
the faster paths and the ones with sharp edges. Human contributors are as much
the audience as agents are.

## Android UniFFI Setup

Fresh worktrees do not contain generated Android UniFFI artifacts because these
paths are ignored by Git:

- `android/app/src/main/kotlin-gen/`
- `android/app/src/main/jniLibs/`
- `target/`

Before running Android JVM tests in a fresh worktree, generate the host
`cruisemesh_core` library and Kotlin bindings:

```powershell
cargo build -p cruisemesh-core --features cruisemesh-core/cli
cargo run -p cruisemesh-core --bin uniffi-bindgen --features cruisemesh-core/cli -- generate --library target/debug/cruisemesh_core.dll --language kotlin --out-dir android/app/src/main/kotlin-gen
cd android
.\gradlew.bat testDebugUnitTest
```

The same host setup runs the fast UI behavior tests and deterministic screenshot
gate:

```powershell
cd android
.\gradlew.bat :app:testDebugUnitTest :app:validateDebugScreenshotTest
```

For an intentional UI change, review regenerated references from
`.\gradlew.bat :app:updateDebugScreenshotTest`. Managed-device UI tests require
the full `core/build-android.sh` path described below; run API 36 with
`.\gradlew.bat :app:pixel6Api36DebugAndroidTest` or minimum-SDK API 31 with
`.\gradlew.bat :app:pixel2Api31DebugAndroidTest`.

On macOS/Linux, replace the library path with the host artifact Cargo produced,
usually `target/debug/libcruisemesh_core.dylib` or
`target/debug/libcruisemesh_core.so`.

Use `core/build-android.sh` instead when Android packaging/native ABI outputs are
needed. That full path also creates `android/app/src/main/jniLibs/`, but requires
Android NDK setup and `cargo-ndk`. It stamps both `kotlin-gen/` and `jniLibs/`
with a matching `.cruisemesh-native-stamp` value on success; Gradle's
`verifyNativeBindingsSync` task (wired into `preBuild`) fails `assembleDebug`
and friends if the two dirs are missing, unstamped, or stamped from different
runs — the quick host-only bindgen command above does NOT write a stamp, so it
alone is enough for JVM unit tests but not for building/running the app.

## iOS

The Swift bindings in `ios/CruiseMesh/Generated/` are checked in, so a core
change that adds or alters an exported function must regenerate them or the iOS
build links against a stale surface. That step needs no Mac — `uniffi-bindgen`
introspects the *host* library, so the same command works on Windows:

```powershell
cargo build -p cruisemesh-core --features cruisemesh-core/cli
cargo run -p cruisemesh-core --bin uniffi-bindgen --features cruisemesh-core/cli -- generate --library target/debug/cruisemesh_core.dll --language swift --out-dir ios/CruiseMesh/Generated
```

`cruisemesh_coreFFI.modulemap` regenerates byte-identical; revert it if it shows
as modified (line endings only). Compiling the Swift itself still needs a Mac or
the `ios.yml` runner.

`core/build-ios.sh` must run on a Mac. From `ios/`, `xcodegen generate` produces
the Xcode project; run the test suite against an available simulator with
`xcodebuild test -project CruiseMesh.xcodeproj -scheme CruiseMesh -destination
"platform=iOS Simulator,id=<simulator-udid>" CODE_SIGNING_ALLOWED=NO`.

## Two-phone BLE smoke test

`tools/two_phone_ble_smoke.sh` drives a real pair of Android phones through
the failure mode that keeps regressing: a LAN link dies and delivery has to
continue over Bluetooth. With two devices attached over adb and the app
installed on both:

```sh
tools/two_phone_ble_smoke.sh -p Emma          # -a/-b to pick serials explicitly
```

It gates on a **delivery receipt in logcat**, never on the chat header. The
header lied for ten minutes during the B5 investigation and turned three good
runs into false failures; screenshots of it are saved as artifacts for a human
to review, but nothing asserts on them. Wi-Fi state is restored on exit even
if the run fails.
