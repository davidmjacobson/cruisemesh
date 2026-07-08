# CruiseMesh

Offline-first family messaging for cruise ships. See [DESIGN.md](DESIGN.md) for the
full design and milestone plan.

## Layout

- `core/` — Rust core (identity, crypto, sync engine) exposed to native shells via
  [UniFFI](https://mozilla.github.io/uniffi-rs/).
- `android/` — Android app (Kotlin, Jetpack Compose), currently Milestone 0/1 scaffold.

## Building

**Core (host, for tests):**

```sh
cargo test -p cruisemesh-core
```

**Core for Android + Kotlin bindings** (requires `rustup target add
aarch64-linux-android armv7-linux-androideabi x86_64-linux-android`, `cargo install
cargo-ndk`, and the Android NDK):

```sh
core/build-android.sh
```

Run this after changing anything in `core/`; it regenerates
`android/app/src/main/kotlin-gen` and `android/app/src/main/jniLibs`.

**Android app:**

```sh
cd android
./gradlew assembleDebug
```
