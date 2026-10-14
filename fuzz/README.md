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
workflow. When `core/` gains or changes a dependency, refresh it with
`cargo update --workspace --manifest-path fuzz/Cargo.toml` and commit the
result in the same change.
