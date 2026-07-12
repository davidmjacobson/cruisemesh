<!-- Thanks! A few checks that save review round-trips. -->

**What this changes and why**

**Checklist**

- [ ] Commits are DCO signed-off (`git commit -s`) — see CONTRIBUTING.md
- [ ] `cargo test --workspace` passes
- [ ] If `core/` changed: bindings regenerated (`core/build-android.sh` / `core/build-ios.sh`)
- [ ] If protocol/architecture changed: DESIGN.md updated in this PR
- [ ] No new cryptographic constructions (CONTRIBUTING.md)
- [ ] User-facing copy stays honest about delivery expectations

**For non-trivial contributions:** comment `I have read and agree to CLA.md`
if you haven't on a previous PR.
