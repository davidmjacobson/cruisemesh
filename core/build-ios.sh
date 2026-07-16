#!/usr/bin/env bash
# Build cruisemesh-core for iOS + generate Swift UniFFI bindings.
# Requires: macOS, Xcode, rustup targets:
#   aarch64-apple-ios, aarch64-apple-ios-sim, x86_64-apple-ios
# Run from anywhere; paths resolve relative to this script.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ios_app="$repo_root/ios/CruiseMesh"
gen_dir="$ios_app/Generated"
xcframework_out="$ios_app/Frameworks/cruisemesh_core.xcframework"

cd "$repo_root"

echo "==> Building host library (for bindgen introspection)"
cargo build -p cruisemesh-core --features cruisemesh-core/cli

host_lib="target/debug/libcruisemesh_core.dylib"
[ -f "$host_lib" ] || host_lib="target/debug/libcruisemesh_core.a"
[ -f "$host_lib" ] || host_lib="target/debug/cruisemesh_core.dll"

echo "==> Generating Swift bindings from $host_lib"
mkdir -p "$gen_dir"
cargo run -p cruisemesh-core --bin uniffi-bindgen --features cruisemesh-core/cli -- \
    generate --library "$host_lib" --language swift --out-dir "$gen_dir"

echo "==> Cross-compiling for iOS device + simulators"
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios >/dev/null 2>&1 || true

cargo build -p cruisemesh-core --release --target aarch64-apple-ios
cargo build -p cruisemesh-core --release --target aarch64-apple-ios-sim
cargo build -p cruisemesh-core --release --target x86_64-apple-ios

device_lib="$repo_root/target/aarch64-apple-ios/release/libcruisemesh_core.a"
sim_arm_lib="$repo_root/target/aarch64-apple-ios-sim/release/libcruisemesh_core.a"
sim_x86_lib="$repo_root/target/x86_64-apple-ios/release/libcruisemesh_core.a"
sim_universal="$repo_root/target/ios-sim-universal/libcruisemesh_core.a"

mkdir -p "$(dirname "$sim_universal")"
lipo -create -output "$sim_universal" "$sim_arm_lib" "$sim_x86_lib"

echo "==> Packaging XCFramework"
rm -rf "$xcframework_out"
mkdir -p "$(dirname "$xcframework_out")"
xcodebuild -create-xcframework \
    -library "$device_lib" \
    -headers "$gen_dir" \
    -library "$sim_universal" \
    -headers "$gen_dir" \
    -output "$xcframework_out"

# Xcode consumes the module map packaged inside the XCFramework. A second
# generic `module.modulemap` beside the Swift source makes modern Xcode define
# `cruisemesh_coreFFI` twice during dependency scanning.
rm -f "$gen_dir/module.modulemap"

echo "==> Done."
echo "    Swift sources:  $gen_dir"
echo "    XCFramework:    $xcframework_out"
echo "    Next: cd ios && xcodegen generate && open CruiseMesh.xcodeproj"
