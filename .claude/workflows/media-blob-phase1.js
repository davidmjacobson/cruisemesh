export const meta = {
  name: 'media-blob-phase1',
  description: 'Implement Phase 1 of the media blob plane: core surface and link plumbing (pure Rust)',
  whenToUse: 'Run from the CruiseMesh-media-two-plane worktree, after the feature freeze lifts, to implement specs/media-two-plane.md Phase 1.',
  phases: [
    { title: 'Scope', detail: 'one agent maps exact insertion points into a task list' },
    { title: 'Implement', detail: 'file-disjoint lanes: record mux, capability bit, BLOB-01 tests; then integration module; then exports + contract flips' },
    { title: 'Verify', detail: 'fmt + full workspace tests, then two adversarial reviewers' },
    { title: 'Fix', detail: 'one repair round if verification is red' },
  ],
}

// Phase 1 of specs/media-two-plane.md (rev 2): everything is pure Rust and
// provable with `cargo test --workspace`. All work happens in the worktree —
// never the main checkout.
const WT = (args && args.worktree) || 'C:/Users/david/CruiseMesh-media-two-plane'

const GROUND = `
You are implementing Phase 1 of specs/media-two-plane.md (rev 2) in the
CruiseMesh repo. Work ONLY in the git worktree at ${WT} (branch
agent/media-two-plane) — never touch C:/Users/david/CruiseMesh. Read
${WT}/specs/media-two-plane.md (especially "Wire protocol" and
"Implementation phases / Phase 1") and ${WT}/core/src/media/mod.rs ("What
integration still owes") before writing code. House rules: core-first; match
surrounding comment density and idiom; run \`cargo fmt --all\` in ${WT}
before finishing; new invariant-shaped behavior gets a test in the style of
its neighbors. Do NOT commit — leave changes in the working tree.
`

phase('Scope')
const plan = await agent(`${GROUND}
Produce a precise task map for the five Phase-1 work items (record mux in
core/src/lan_session.rs; CAP_MEDIA_BLOB in core/src/protocol.rs; the
integration module owed by core/src/media/mod.rs items 1-3 and 5; UniFFI
exports; BLOB-01 adversarial cases + contract owner flips in
core/tests/protocol_contract.rs and specs/protocol-contract-v1.md). For each:
the exact files, functions, and line-anchored insertion points; the existing
tests to mirror; and any ordering hazard between lanes (especially who may
touch core/src/lib.rs — reserve it for the integration/exports lanes only).
Read code; change nothing.`, {
  label: 'scope', phase: 'Scope', model: 'opus',
  schema: {
    type: 'object',
    required: ['mux', 'capability', 'blob01_tests', 'integration', 'exports'],
    properties: {
      mux: { type: 'string' }, capability: { type: 'string' },
      blob01_tests: { type: 'string' }, integration: { type: 'string' },
      exports: { type: 'string' }, hazards: { type: 'string' },
    },
  },
})

phase('Implement')
// Lane A/B/C are file-disjoint and run concurrently. lib.rs is off-limits to
// all three (the scope agent's hazard notes repeat this).
await parallel([
  () => agent(`${GROUND}
Task map from the scoping pass:\n${plan.mux}\nHazards:\n${plan.hazards || 'none noted'}
Implement RECORD_TYPE_BLOB = 2 in core/src/lan_session.rs per the spec's
"LAN sub-channel: multiplexing" section: per-record-type single-in-flight
reassemblers, independent frame_id spaces, mesh-priority at the send queue's
seam (encrypt_frame gains a record-type parameter or a blob-flavored twin —
follow the module's existing shape), and unknown-record-type rejection
pinned by test. Touch ONLY core/src/lan_session.rs (and its tests).`,
    { label: 'impl:record-mux', phase: 'Implement', model: 'opus' }),
  () => agent(`${GROUND}
Task map from the scoping pass:\n${plan.capability}
Add CAP_MEDIA_BLOB = 1 << 5 to core/src/protocol.rs, OR it into
core_own_capabilities(), and add a bit-allocation test in the style of
the_own_roster_notice_has_its_own_capability_bit. Touch ONLY
core/src/protocol.rs.`,
    { label: 'impl:capability', phase: 'Implement', model: 'opus' }),
  () => agent(`${GROUND}
Task map from the scoping pass:\n${plan.blob01_tests}
Add adversarial BLOB-01 cases to the spray and carry test suites: prove no
blob byte, chunk, or pull frame can enter an envelope, a carry queue, a
digest spray plan, or a BLE frame (type-level unreachability is the design;
the tests make it observable — e.g. a media manifest message sprays as an
ordinary bounded envelope while its blob does not). Touch ONLY test code in
the spray/carry suites the scope map names.`,
    { label: 'impl:blob01-tests', phase: 'Implement', model: 'opus' }),
])

// Lane D depends on A+B (uses the record type and the capability bit).
await agent(`${GROUND}
Task map from the scoping pass:\n${plan.integration}
Lanes just landed in this worktree: the record mux in lan_session.rs and
CAP_MEDIA_BLOB in protocol.rs — build on them, do not rework them.
Implement the integration module core/src/media/mod.rs owes (checklist items
1, 2, 3, 5): authoring (seal_blob + encode_media_manifest into a
KIND_ATTACHMENT_MANIFEST body), receive-side manifest recognition opening a
BlobStore row, MEDIA_SCHEMA_SQL applied on the MessageStore connection with
the backup posture from the spec (metadata backs up, chunk files do not),
and the blob-transfer consent verdict composed from
core_relay_network_permitted (LAN always permitted; relay paths inherit the
roaming/constrained verdict). Pure, table-tested policy; a new module under
core/src/media/ plus minimal seams elsewhere. You may touch core/src/lib.rs
for module declarations only.`,
  { label: 'impl:integration', phase: 'Implement', model: 'opus' })

// Lane E depends on everything: exports + contract owner flips.
await agent(`${GROUND}
Task map from the scoping pass:\n${plan.exports}
All Phase-1 implementation lanes have landed in this worktree. Export the
new surface over UniFFI (follow AGENTS.md for the bindgen recipe; regenerate
bindings so both shells compile — kotlin-gen/jniLibs setup notes are in
CLAUDE.md/AGENTS.md). Then flip BLOB-01 and BLOB-03 from unimplemented to
core-owned in core/tests/protocol_contract.rs AND the matching owner-class
cells in specs/protocol-contract-v1.md, naming the real tests the other
lanes added — the cross-check tests in protocol_contract.rs must pass.`,
  { label: 'impl:exports-contract', phase: 'Implement', model: 'opus' })

phase('Verify')
const build = await agent(`${GROUND}
Run \`cargo fmt --all\` then \`cargo test --workspace\` in ${WT} (the full
workspace: core, relayd, AND desktop — a core export change that breaks a
desktop call site must be caught here). Report every failure verbatim. Fix
nothing.`,
  { label: 'verify:workspace', phase: 'Verify', model: 'opus', schema: {
    type: 'object', required: ['green', 'report'],
    properties: { green: { type: 'boolean' }, report: { type: 'string' } },
  } })

const reviews = await parallel([
  () => agent(`${GROUND}
Adversarially review the uncommitted diff in ${WT} (git diff) through the
plane-separation lens: can any path move blob bytes into an envelope, carry
queue, spray plan, or BLE frame? Is the pull proof verified before any chunk
is served? Does the consent verdict really compose with (not duplicate)
core_relay_network_permitted? Try to construct a concrete violation.`,
    { label: 'review:plane-separation', phase: 'Verify', model: 'opus', schema: {
      type: 'object', required: ['findings'],
      properties: { findings: { type: 'array', items: { type: 'object',
        required: ['file', 'summary', 'severity'],
        properties: { file: { type: 'string' }, summary: { type: 'string' },
          severity: { enum: ['blocker', 'minor'] } } } } },
    } }),
  () => agent(`${GROUND}
Adversarially review the uncommitted diff in ${WT} (git diff) through the
wire-compatibility lens: does an OLD peer (no CAP_MEDIA_BLOB, no record type
2) survive every new frame and record unchanged? Legacy HELLO must not grow
trailing fields; record-type-2 rejection must keep the link; frame 0x08 and
capability bits above 5 must remain unallocated; the manifest codec must be
byte-compatible with the dark module's pinned tests. Try to construct a
concrete break.`,
    { label: 'review:wire-compat', phase: 'Verify', model: 'opus', schema: {
      type: 'object', required: ['findings'],
      properties: { findings: { type: 'array', items: { type: 'object',
        required: ['file', 'summary', 'severity'],
        properties: { file: { type: 'string' }, summary: { type: 'string' },
          severity: { enum: ['blocker', 'minor'] } } } } },
    } }),
])

const blockers = reviews.filter(Boolean).flatMap(r => r.findings).filter(f => f.severity === 'blocker')

phase('Fix')
if (!build.green || blockers.length > 0) {
  const repair = await agent(`${GROUND}
Repair round. Test report:\n${build.report}\nBlocker findings:\n${JSON.stringify(blockers, null, 2)}
Fix every failure and blocker, then run \`cargo fmt --all\` and
\`cargo test --workspace\` in ${WT} yourself and report the final state.`,
    { label: 'fix', phase: 'Fix', model: 'opus', schema: {
      type: 'object', required: ['green', 'report'],
      properties: { green: { type: 'boolean' }, report: { type: 'string' } },
    } })
  return { green: repair.green, report: repair.report, reviews: reviews.filter(Boolean).flatMap(r => r.findings) }
}

log('Workspace green, no blockers')
return { green: true, report: build.report, reviews: reviews.filter(Boolean).flatMap(r => r.findings) }
