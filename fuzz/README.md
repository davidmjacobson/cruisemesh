# Core decoder fuzzing

These targets exercise the byte-oriented, pre-authentication surfaces called
out by the adversarial payload review. Install `cargo-fuzz` and a Rust nightly,
then run a target from the repository root, for example:

```text
cargo +nightly fuzz run protocol_decoders
cargo +nightly fuzz run relay_wire
cargo +nightly fuzz run ble_reassembly
cargo +nightly fuzz run noise_handshake
cargo +nightly fuzz run backup_wire
```

The generated corpus and crash artifacts stay untracked. Promote any minimal
crasher into a deterministic core regression test before fixing it.

`fuzz/` is its own workspace with its own committed `Cargo.lock`, audited by
the dependency-audit workflow and enforced as locked by the fuzz-smoke
workflow. It is derived from the root `Cargo.lock`, so the fuzzers pin the
same versions core is tested and audited with. Whenever the root lockfile
changes in a way that touches core's graph (a new or changed dependency in
`core/Cargo.toml`, or a `cargo update -p <crate>` for an advisory), rebuild it
from root and commit the result in the same change:

```sh
cp Cargo.lock fuzz/Cargo.lock
cargo metadata --format-version 1 --manifest-path fuzz/Cargo.toml > /dev/null
```

`cargo metadata` prunes what fuzz/ does not use and adds the fuzz-only crates.
Do not use `cargo update --workspace` here: it leaves crates core already
pulls in at their old versions, so the fuzz graph drifts from root.
