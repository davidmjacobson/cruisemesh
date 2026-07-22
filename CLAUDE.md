# CLAUDE.md

Load-bearing session knowledge. Build/bindgen detail: `AGENTS.md`.

- **Worktree per task, never the main checkout**: `git worktree add
  ../CruiseMesh-<slug>`, one branch per item (`agent/<slug>`), off `master`.
- **Fresh-worktree Android setup**: `kotlin-gen/` and `jniLibs/` are
  gitignored — copy both from the main checkout, then `cargo build` at the
  repo root, before Android JVM unit tests (see `AGENTS.md` for the full
  bindgen recipe).
- **Commit author email** must be
  `14227840+davidmjacobson@users.noreply.github.com` or the push is rejected.
- **Tests**: core `cargo test -p cruisemesh-core`; workspace
  `cargo test --workspace`; Android `cd android && ./gradlew
  :app:testDebugUnitTest --rerun-tasks`; iOS builds/tests need a Mac (setup
  notes live outside the repo — ask before starting iOS work).
- **Core-first**: shared behavior lives in the Rust core (`core/src/`),
  exported via UniFFI; never fix it in one platform's shell. Pure
  schedule/policy logic = plain classes with no Android imports, unit-tested
  directly.
- **Strings in resources**: user-facing copy goes in `strings.xml` /
  `Localizable.xcstrings`; CI rejects hardcoded literals. Sentence case,
  literal status/error copy, no protocol jargon.
- **Endpoint privacy invariant**: each phone advertises ONLY its own endpoint,
  sealed pairwise per contact. Discovered or third-party IPs are never
  forwarded to anyone.
- **DTN ack safety invariant**: never ack a relay copy unless this device was
  the envelope's sole true endpoint consumer; a carried 1:1 envelope is
  removed only on digest-proof of receipt, never on dispatch. When in doubt,
  don't ack.
- **Product bar**: obvious for family members on the surface; capability for
  power users behind Advanced. When in tension, simplicity wins the surface.
