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
cargo run -p cruisemesh-core --bin uniffi-bindgen --features cruisemesh-core/cli -- generate --no-format --library target/debug/cruisemesh_core.dll --language swift --out-dir ios/CruiseMesh/Generated
```

`--no-format` is not optional. Without it `uniffi-bindgen` pipes the generated
Swift through `swiftformat` **if that binary happens to be on `PATH`** — which it
is on a Mac with the Homebrew formula installed, and is not on the Linux runner
that gates the result. `swiftformat` rewraps and reorders, so the difference is
not whitespace and the `--ignore-all-space` below would not absorb it: someone
who has it installed would regenerate in good faith and be told their bindings
are stale, the single worst message a tripwire can give. The flag makes every
generation path emit the same bytes. `core/build-ios.sh` and `rust.yml` pass it
for the same reason.

`cruisemesh_coreFFI.modulemap` regenerates byte-identical; revert it if it shows
as modified (line endings only). Compiling the Swift itself still needs a Mac or
the `ios.yml` runner.

### Never hand-edit `ios/CruiseMesh/Generated/`

Regenerate, always — including for a change that looks cosmetic. UniFFI verifies
a per-function checksum at *runtime*, so a stale or edited binding is not a
compile error: it is a `fatalError` the moment the app launches. Since UniFFI
0.28.3 a function's doc comment feeds that checksum, so even a comment-only edit
in `core/` invalidates the committed Swift.

Both halves of that have already happened here. Master carried a stale Swift
binding for several days — a doc-comment edit to `core_transport_send_plan` moved
its checksum with nothing regenerated — and it was found by hand while adding the
gate below (#269), not by any check; the next iOS release build would have died at
launch. The Android analogue got further: `jniLibs/` drifting from `kotlin-gen/`
shipped an APK that crashed on launch with `UnsatisfiedLinkError`, which is why
that pair now carries a matching build-time stamp (#112).

`rust.yml` holds the one authoritative gate on this. It regenerates the bindings
and then runs, blocking, on every PR with no path filter:

```powershell
git diff --exit-code --ignore-all-space ios/CruiseMesh/Generated
```

`--ignore-all-space` is load-bearing: the committed copies are the generator's
output with trailing whitespace stripped, which is what the editors in this
project do on save. Signature and checksum drift — the thing that crashes the app
— is not whitespace, so it still fails. `ios.yml` deliberately carries no drift
check of its own; it regenerates before compiling, so it proves the complementary
half (a freshly generated binding builds and its tests pass). Do not add a second
gate there.

Reproduce the gate locally with the two generation commands above followed by
that `git diff`. To confirm it still has teeth, *commit* a one-character change to
a checksum constant in `Generated/cruisemesh_core.swift`, regenerate, and check
the diff comes back non-empty — a working-tree-only edit proves nothing, since
regeneration overwrites it.

Drift is only half the problem: a binding that matches can still marshal a value
wrongly, and nothing compiled catches that either.
`android/app/src/test/kotlin/com/cruisemesh/app/mesh/CoreBindingSmokeTest.kt` and
`ios/CruiseMeshTests/CoreBindingSmokeTests.swift` execute the boundary itself —
enum discriminants, optionals present and absent, byte arrays, nested record
round trips — as *shape* checks only. When an exported enum or record lands with
a shape those files do not already cover, extend them. Never restate in them the
policy the core's own tests own.

They are not a second drift check and cannot stand in for one. Both shells build
their bindings fresh before running tests — Android's are gitignored, and
`ios.yml` regenerates `Generated/` in `core/build-ios.sh` before `xcodebuild` — so
neither suite ever loads the *committed* Swift. The smokes catch marshalling bugs;
only the `rust.yml` diff catches a checked-in binding going stale. Nothing else
stands between that and a launch-time `fatalError` on TestFlight.

## iOS build and simulator

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
