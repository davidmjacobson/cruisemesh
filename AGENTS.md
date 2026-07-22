# Agent Notes

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

`core/build-ios.sh` must run on a Mac. From `ios/`, `xcodegen generate` produces
the Xcode project; run the test suite against an available simulator with
`xcodebuild test -project CruiseMesh.xcodeproj -scheme CruiseMesh -destination
"platform=iOS Simulator,id=<simulator-udid>" CODE_SIGNING_ALLOWED=NO`.
