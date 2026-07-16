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
Android NDK setup and `cargo-ndk`.

## macOS Validation Host

This Windows workstation has a machine-local SSH alias for the macOS validation
host. Connect without putting host coordinates or key paths in tracked files:

```powershell
ssh cruisemesh-mac-validation
```

The remote repository is `~/cruisemesh`. Treat that checkout as user-owned: run
`git status -sb` first, do not discard its changes or generated artifacts, and
perform validation in a sibling worktree at the exact commit under test:

```bash
cd ~/cruisemesh
git fetch origin <branch>
git worktree add --detach ~/cruisemesh-<task> FETCH_HEAD
```

Rust and XcodeGen are installed in user-local directories. Non-interactive SSH
commands may need this path explicitly:

```bash
export PATH="$HOME/.cargo/bin:$HOME/.local/opt/xcodegen-2.45.4/xcodegen/bin:$PATH"
```

The port-2222 listener is a user-owned `sshd`, not the shared Mac's system
service. If the VNC/GUI login expires and the alias refuses connections, use
the provider's system-SSH channel and account credentials supplied out of band,
then validate and start the existing configuration in daemon mode:

```bash
config="$HOME/.local/var/user-sshd/sshd_config"
log="$HOME/.local/var/user-sshd/sshd.log"
/usr/sbin/sshd -t -f "$config"
/usr/sbin/sshd -f "$config" -E "$log"
/usr/sbin/lsof -nP -iTCP:2222 -sTCP:LISTEN
```

Do not add `-D` for this recovery command: normal daemon mode detaches the
listener from the temporary system-SSH session and the expiring GUI login.

From the temporary worktree, build the shared core, generate the Xcode project,
and run the iOS suite against an available simulator:

```bash
bash core/build-ios.sh
cd ios
xcodegen generate
xcrun simctl list devices available
xcodebuild test \
  -project CruiseMesh.xcodeproj \
  -scheme CruiseMesh \
  -destination "platform=iOS Simulator,id=<simulator-udid>" \
  CODE_SIGNING_ALLOWED=NO
```

After validation, remove only the temporary worktree. On this host, `pwd -P`
resolves `/Users/...` through `/Volumes/Macintosh_HD/Users/...`; account for that
when verifying a path before any forced worktree removal. Never clean or remove
the original `~/cruisemesh` checkout.
