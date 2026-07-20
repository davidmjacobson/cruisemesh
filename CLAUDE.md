# CLAUDE.md

Load-bearing session knowledge. Full detail: `AGENTS.md`, `TODO.md` §3,
`AGENT-TODO.md` (plan of record), `fable-todo.md` (audit backlog).

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
  :app:testDebugUnitTest --rerun-tasks`; iOS on the Mac validation host (see
  `AGENTS.md` for the SSH/worktree runbook — check reachability first).
- **Strings in resources**: user-facing copy goes in `strings.xml` /
  `Localizable.xcstrings`; CI rejects hardcoded literals.
- **Endpoint privacy invariant**: TODO.md §3.5.
- **DTN ack safety invariant**: TODO.md §3.6.
