#!/usr/bin/env bash
# Rebuilds the Rust core for Android: cross-compiles cruisemesh-core for all
# target ABIs via cargo-ndk, then regenerates the Kotlin/JNA bindings from a
# host build. Run from anywhere; paths are resolved relative to this script.
# The release workflow separately checks every packaged 64-bit ELF for 16 KiB
# PT_LOAD alignment before publishing an APK or AAB.
#
# Requires: rustup targets aarch64-linux-android, armv7-linux-androideabi,
# x86_64-linux-android; cargo-ndk (`cargo install cargo-ndk`); ANDROID_NDK_HOME
# set (or ANDROID_HOME/ndk/<version> present).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
android_app="$repo_root/android/app/src/main"

if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    ndk_dir="$(find "${ANDROID_HOME:-$repo_root/../Sdk}/ndk" -maxdepth 1 -mindepth 1 -type d 2>/dev/null | sort -V | tail -1)"
    if [ -z "$ndk_dir" ]; then
        echo "error: set ANDROID_NDK_HOME (no NDK auto-detected)" >&2
        exit 1
    fi
    export ANDROID_NDK_HOME="$ndk_dir"
fi
echo "Using ANDROID_NDK_HOME=$ANDROID_NDK_HOME"

cd "$repo_root"

echo "==> Building host library (for bindgen introspection)"
cargo build -p cruisemesh-core --features cruisemesh-core/cli

host_lib="target/debug/cruisemesh_core.dll"
[ -f "$host_lib" ] || host_lib="target/debug/libcruisemesh_core.so"
[ -f "$host_lib" ] || host_lib="target/debug/libcruisemesh_core.dylib"

echo "==> Generating Kotlin bindings from $host_lib"
mkdir -p "$android_app/kotlin-gen"
cargo run -p cruisemesh-core --bin uniffi-bindgen --features cruisemesh-core/cli -- \
    generate --library "$host_lib" --language kotlin --out-dir "$android_app/kotlin-gen"

echo "==> Cross-compiling for Android ABIs (arm64-v8a, armeabi-v7a, x86_64)"
cargo ndk \
    -t arm64-v8a -t armeabi-v7a -t x86_64 \
    -o "$android_app/jniLibs" \
    build --release -p cruisemesh-core

# Stamp both output dirs with a value unique to this run, so Gradle
# (verifyNativeBindingsSync in android/app/build.gradle.kts) can fail the
# build fast if kotlin-gen/ and jniLibs/ ever drift apart (e.g. one
# regenerated without the other) instead of shipping an APK that crashes at
# launch with a UniFFI checksum mismatch.
stamp="$(date -u +%Y%m%dT%H%M%SZ)-$$-$RANDOM"
echo "$stamp" > "$android_app/kotlin-gen/.cruisemesh-native-stamp"
echo "$stamp" > "$android_app/jniLibs/.cruisemesh-native-stamp"

echo "==> Done. Kotlin bindings: $android_app/kotlin-gen"
echo "              Native libs: $android_app/jniLibs"
