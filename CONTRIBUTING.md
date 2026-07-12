# Contributing to CruiseMesh

Thanks for your interest! CruiseMesh is an offline-first, end-to-end-encrypted
family messenger (Rust core, native iOS/Android shells, self-hostable relay).
Start with [DESIGN.md](DESIGN.md) — most architectural questions are answered
there, and PRs that fight the design doc will be asked to argue with the doc
first.

## License and contribution terms

CruiseMesh is licensed under the **GNU AGPL-3.0-or-later** (see
[LICENSE](LICENSE)).

Two requirements apply to contributions:

1. **DCO sign-off (all contributions).** Every commit must be signed off:

   ```sh
   git commit -s
   ```

   This adds a `Signed-off-by:` line certifying the
   [Developer Certificate of Origin](https://developercertificate.org/) —
   that you wrote the change or otherwise have the right to submit it under
   the project license. PRs with unsigned commits can't be merged.

2. **CLA (non-trivial contributions).** For contributions beyond small fixes
   (roughly: anything that would be copyrightable on its own), we ask you to
   agree to the lightweight Contributor License Agreement in
   [CLA.md](CLA.md). It keeps the project able to make licensing decisions
   as a whole (for example, releasing under a later AGPL version, or
   offering the core under an additional license) without tracking down
   every past contributor. You keep full rights to your own work. Agreement
   is recorded by a checkbox comment on your first qualifying PR:
   `I have read and agree to CLA.md`.

If either requirement is a dealbreaker for you, open an issue and say so —
feedback on the terms is welcome.

## Building and testing

See [README.md](README.md) for full build instructions. The short version:

```sh
cargo test --workspace        # Rust core + relayd (run this before every PR)
core/build-android.sh         # regenerate Kotlin bindings after core changes
core/build-ios.sh             # regenerate Swift bindings (macOS + Xcode)
```

If you change anything in `core/`, the generated bindings and JNI libs must
be regenerated or CI will disagree with you.

## What makes a good PR here

- **Small and single-purpose.** The sync engine and crypto paths are subtle;
  reviewability is a feature.
- **Tests for protocol behavior.** Anything touching envelopes, receipts,
  dedupe, or sync digests needs a headless test in `core/` — that's how this
  project stays trustworthy without two phones in hand (DESIGN.md §10).
- **No new cryptographic constructions.** We use libsodium primitives whole
  (DESIGN.md §6.1). PRs that assemble crypto primitives in novel ways will
  be declined regardless of quality — this is a load-bearing project rule.
- **Match the design doc, or change it first.** If your change alters
  protocol or architecture, PR a DESIGN.md update as part of the change.
- **Honest UX copy.** User-facing text must not promise coverage or
  instant delivery; this app's credibility rests on truthful expectations.

## Reporting bugs and proposing features

Use the issue templates. For anything security-relevant, **do not open a
public issue** — see [SECURITY.md](SECURITY.md).

## Field reports

Real-world reports (cruise ships, hikes, festivals — anywhere BLE and
patience got tested) are uniquely valuable to this project. There's a
dedicated issue template; delivery-mode mix, latency, battery, and device
models are the gold.
