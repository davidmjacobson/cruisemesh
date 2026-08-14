# Multi-device support v1

Status: **draft 2026-08-11, pending the three decisions in §14**. Everything
outside §14 is settled design, ready to implement in the work-package order
of §13. Derived from the 2026-08-11 design review (working notes in
`specs/multi-device.md`).

Scenario: one person, several devices. Alice has a Windows desktop and an
Android phone; Bob has an iPad and an iPhone. Alice messages "Bob" — one
contact, all of Bob's devices receive it, and Alice's own devices converge
on the same history. Modeling a person's devices as a pseudo-group is
explicitly rejected: device count must be invisible to other users, and
group semantics (membership, receipts, fan-out) are the wrong shape.

---

## 1. Problem

Today identity **is** the device: one Ed25519 keypair = one `user_id` = one
contact = one endpoint. Consequences:

- A second device means a second identity: re-friending everyone, split
  history, two chat threads per contact.
- The tempting shortcut — restoring a `.cmbak` backup onto a second *live*
  device — creates two devices signing the same author stream. They mint
  colliding lamports while offline, and the store's per-stream conflict
  policy (`core/src/store.rs`, `insert_message` →
  `incoming_message_reference::insert`) classifies the siblings' rows as a
  fork and quarantines one side's history. This is the #1 prerequisite to
  fix before two devices may author as one person (§5).
- The relay mailbox row for a 1:1 message has exactly one true consumer.
  With two recipient devices sharing an identity, the first device to
  fetch-and-ack deletes the only copy and starves the other — the
  multi-device shape of the DTN ack-safety invariant.

## 2. Goals / non-goals

Goals:

1. One contact per person. Contacts, conversations, groups, and safety
   fingerprints stay keyed by the person; other users never see device
   count or device identity.
2. Existing identities upgrade in place: the deployed Ed25519 key becomes
   the person root, so `Contact.user_id`, chat ids, and fingerprints keep
   working with **no re-friending**.
3. All four transports (relay, LAN, BLE, carry) work for both
   contact-facing traffic and own-device sync; two of a person's devices
   never need to meet directly.
4. A lost or stolen device can be cut off (roster revocation + key and
   credential rotation) without rebuilding the identity.
5. Legacy (single-device) builds interoperate indefinitely: they see a
   multi-device person as one contact and are never starved or forked.

Non-goals (v1):

- Group crypto changes. Groups keep the shared symmetric key and existing
  per-member relay fan-out (§11). Pairwise/sender-key group crypto is a
  separate future track.
- Relay server schema changes. relayd is content-agnostic and needs none
  for v1 (verified: `relayd/src/lib.rs` scopes rows by
  `(family_token, msg_id, recipient_hint)` only). Per-device relay
  credentials ("relay v2") are deliberately deferred (§10, §13 WP8).
- Cloud anything. Self-sync is mesh traffic, not a hosted service.
- Serverless identity recovery beyond the recovery code. "Lost every
  device and the recovery code" = new identity, re-friend; any override
  path would itself be the vulnerability.

## 3. Identity model

**Person root.** The existing Ed25519 identity key becomes the *person
root*; its public key remains the wire `user_id` (now also called
`person_id` in new code). The root signs device certificates and roster
genesis — after migration it is never again used to sign messages.

**Devices.** Each device holds its own Ed25519 signing key and X25519 DH
key. A *device certificate* binds `(person_id, device_id = device signing
pubkey, added_epoch, cert flags)` under a person-authorized signature.
All new-format signatures are domain-separated (distinct context strings
for device certs, roster updates, message authoring, sync records) so a
signature from one domain can never be replayed in another.

**Authority split (who may change the roster).** Message authoring never
requires any special device. Roster changes (add/revoke device) require
one of:

- the **approving-device key**: exactly one linked device holds the
  roster-signing role at a time (by default, the first/oldest device);
- the **recovery code**: an offline secret shown once at identity
  creation or upgrade (extends the existing `.cmbak` passphrase habit).
  Material derived from it can sign a roster at a **higher recovery
  epoch**, which always supersedes anything the approving device signed —
  this is how a stolen approving device is dethroned.

The person root secret is kept only inside the recovery material after
migration; it is **never** copied to every device. (Root-on-every-device
is rejected: a thief holding any phone could revoke the real devices and
hijack the person.)

## 4. Device roster

The roster is a person-signed, versioned document listing current device
certs and revocation tombstones:

```
Roster {
  person_id,
  recovery_epoch,        // bumped only by recovery-code signatures
  seq,                   // monotone within a recovery_epoch
  devices: [DeviceCert], // active
  tombstones: [(device_id, revoked_at_seq)],
  approving_device_id,
  inbox_key_generation,  // §6; bumped on every revocation
  signature,             // approving-device or recovery-derived key
}
```

Roster rules (cite as DL-n in code comments):

- **DL-1 (monotonicity).** A contact accepts a roster only if
  `(recovery_epoch, seq)` strictly exceeds the stored one and the
  signature chain verifies back to the person root. Lower or equal
  versions are ignored (idempotent gossip).
- **DL-2 (fork quarantine).** Two verified rosters with the same
  `(recovery_epoch, seq)` but different content = fork. Keep the stored
  one, quarantine further roster updates for that person, and surface the
  same safety-warning treatment as a changed fingerprint. Never
  auto-resolve a fork.
- **DL-3 (no directory).** Rosters gossip exactly like other sealed 1:1
  traffic — relay, LAN, BLE, and carry equally, sealed pairwise per
  contact. There is no central roster service and the relay never sees
  roster plaintext.
- **DL-4 (tombstones are forever).** A revoked `device_id` never returns
  to `devices`; re-linking the same physical hardware mints a fresh key.
- **DL-5 (endpoint privacy is unchanged).** Rosters carry keys, never
  endpoints. Each device still advertises only its own endpoint, sealed
  pairwise per contact; nothing in this spec forwards discovered or
  third-party addressing to anyone.

## 5. Message identity: per-device author streams

The logical message stream key gains a device dimension:

```
(chat_id, sender_person_id, sender_device_id, lamport)
```

- Store migration adds `sender_device_id` to the message tables and the
  conflict/fork discriminator; per-stream quarantine now operates per
  *device* stream, so two of Alice's devices authoring offline can never
  fork each other.
- Legacy envelopes (no device field) map to the reserved
  `LEGACY_DEVICE_ID` (all-zero) stream of that person. Every existing row
  migrates to it; every v1 peer looks like a one-device person on that
  stream. All existing store tests must stay green under this synthetic
  view.
- **Replies and reactions reference the logical event id** (stream key
  or a stable hash of it), never a transport `msg_id` — the same logical
  message legitimately has many transport ids under fan-out (§7).
- Ordering for display merges the person's device streams by
  `(lamport, tiebreak = device_id)`; lamports are still per-chat and
  advance on receipt as today.

Envelope compatibility: the **public header layout is unchanged**. The
`sender_device_id` and roster references ride inside the sealed body as
new optional fields; legacy receivers ignore what they don't parse
(verified and pinned by WPT, §13 — not assumed), new receivers treat
their absence as `LEGACY_DEVICE_ID`.

## 6. Sealing model (decision D1 — see §14.1)

Different paths have different scarce resources; the sealing model
follows the resource, not symmetry:

- **BLE / carry / spray: one copy, person-sealed.** All linked devices of
  a person hold a person-scoped X25519 **inbox key** (distributed via
  link bootstrap and self-sync, versioned by `inbox_key_generation`).
  Constrained paths carry ONE person-hinted, inbox-sealed copy; whichever
  device receives it first spreads it to siblings via self-sync (§8).
  Rationale (verified): the hint budget allows only ~28 routing
  identities under relayd's `MAX_FETCH_HINTS = 256`
  (`core/src/recipient_hints.rs:249`), and strict per-device sealing
  multiplies every carried copy through third-party mules — BLE duty
  cycle, spray byte budgets, and mule storage are the scarce resources
  here.
- **Relay: per-device rows** (§7). Cheap on the relay, and buys exact
  single-consumer acks.
- **Mandatory inbox rotation on every revocation** (§10). The accepted
  residual: a stolen device reads *new* inbound sealed to the old inbox
  key until the revocation propagates to the sender — the same
  propagation-bounded window strict per-device sealing has, since stale
  contacts seal to the roster they know, stolen device included.
- Escalation path: if the threat model hardens, per-device sealing
  everywhere is the documented fallback; only this section and the cost
  tables change, the rest of the design survives.

## 7. Relay fan-out and ack rules

Mirror the group fan-out machinery, per recipient device:

- `device_fanout_msg_id(original_msg_id, device_id)` — deterministic,
  16 bytes, same construction discipline as the existing group
  `fanout_msg_id(original_msg_id, member_user_id)`
  (`core/src/protocol.rs:1353`, spec'd in
  `specs/group-relay-durability.md` §4.1).
- One relay row per recipient device, each with that device's daily
  recipient hint (per-device hint namespace derived from
  `(person_id, device_id)`); the sender's *own* sibling devices get rows
  too, so relay-reachable siblings converge without a separate channel.
- Each row has exactly **one true consumer**, so the existing consumed-ack
  rule generalizes cleanly through `core_should_ack_inbound`
  (`core/src/engine.rs:350`).

Ack rules (cite as ACK-MD-n; these extend, never replace, the DTN
ack-safety invariant — when in doubt, don't ack):

- **ACK-MD-1.** A device acks (deletes) only rows addressed to its own
  `device_fanout_msg_id` namespace, and only on `CONSUMED`.
- **ACK-MD-2 (mixed fleet).** A multi-device recipient NEVER acks a
  legacy person-addressed 1:1 row. A legacy sender uploads exactly one
  such row; the first sibling to fetch it must leave it for the others
  and propagate content via self-sync. The churn cost is bounded by the
  existing 7-day row expiry and ends when the sender upgrades.
- **ACK-MD-3.** Carried copies keep the existing rule unchanged: a
  carried 1:1 envelope is removed only on digest-proof of receipt, never
  on dispatch, and person-sealed carried copies (§6) are never acked by
  any single device on behalf of the person.

## 8. Self-sync (own devices as a private mesh)

A person's devices converge through signed **sync records** sealed to own
devices only — same envelope machinery, same four transports, new sealed
kinds:

- Record kinds: message history (authored + received), delivered/read
  watermarks, contact list + contact rosters, own roster + inbox keys,
  group membership/state, settings the product deems shared.
- **SYNC-1 (anti-entropy).** Devices exchange compact digests
  (per-stream watermarks) and fill gaps — the existing digest-based
  patterns generalize; no operation may assume both devices are ever
  concurrently online.
- **SYNC-2 (outbound dedup).** An outgoing message is authored once, by
  one device, in that device's stream. Sync must make a sibling aware of
  a pending outbound before it re-authors ("send from whichever device
  is in hand" edits the draft, not the stream); identical re-uploads of
  an already-posted row are safe under relayd's msg_id dedup, but two
  distinct authored copies of the same text are a product bug.
- **SYNC-3 (person boundary).** Sync records are sealed strictly to the
  person's own current device set and re-sealed on roster change. They
  may contain contacts' data the person already legitimately holds
  (cards, endpoints, history) — that data never transits any third
  party's device unsealed and never widens beyond the person boundary
  (DL-5).
- Surface UX: delivered/read shown to contacts is **any-device**;
  per-device receipt detail lives behind Advanced. Send/receive UX never
  blocks on all-device sync.

## 9. Linking a new device

Deep-link route `CMLINK1:` (registered alongside the existing
`CMFRIEND*` routes in `core/src/deep_link.rs`):

1. New device shows a QR: `CMLINK1:` + **ephemeral** link material only
   (a fresh DH pubkey + relay/LAN rendezvous hints). Identity secrets
   NEVER ride the QR.
2. Existing (approving) device scans, opens a Noise channel over
   LAN/BLE/relay, both screens show a short authentication string; the
   user confirms match **on the existing device** (explicit tap).
3. Approving device streams the **canonical bootstrap** — a versioned
   export (identity material incl. inbox key, contacts + their rosters,
   group state, recent history head) — NOT a raw sqlite clone; the rest
   of history arrives as ordinary self-sync catch-up.
4. **Two-phase activation.** The approving device signs the new roster
   (seq+1) including the new cert. The new device may not advertise,
   author, or ack ANYTHING until it (a) has imported the bootstrap and
   (b) has acknowledged the exact new roster hash back to the approving
   device. Until then it is invisible on the mesh.
5. Roster gossips to contacts per §4; senders start adding a relay row
   and (eventually) hints for the new device as the roster reaches them.

Restore-from-backup UX changes to match: opening a `.cmbak` on a fresh
install offers **"Replace this device"** (old semantics, same device
count) or **"Link as new device"** (routes into this ceremony). A raw
clone running live alongside its source is the §1 failure mode and gets
actively detected (two active devices presenting the same `device_id` =
immediate safety warning on siblings).

## 10. Revocation

On "Remove device" (approving device) or recovery-code override:

1. Roster update: tombstone the device, bump `seq` (or `recovery_epoch`
   for recovery-code path), bump `inbox_key_generation`, rotate the inbox
   key, gossip to all contacts and remaining own devices.
2. **Rotate the shared relay `family_token`** and push it via the
   existing relay-update machinery (`CAP_RELAY_UPDATE` paths). Verified
   hole this closes: relayd scopes fetch/ack/delete by `family_token`
   alone, so a revoked device holding the old token could fetch and
   *delete* siblings' rows indefinitely.
3. Receivers refuse signatures from a tombstoned `device_id` on newly
   received events (already-stored history stays; the stream is sealed
   at its last pre-revocation point).
4. Contacts get the standard changed-safety-state surface treatment.

Long-term (out of v1, tracked as WP8): per-device opaque fetch/ack
capabilities in relayd, so one device can be cut without rotating the
whole family token.

## 11. Groups: unchanged in v1

`member_user_ids` remain person ids; group crypto (shared symmetric key)
and per-member relay fan-out are untouched. A member's new device obtains
the group key and state via that member's own self-sync — no re-invites,
no M×D sender-side fan-out (rejected: quadratic). Group receipts shown to
the group stay person-level (any-device, §8). Member-removal/rekey
remains its own future track; sender-key designs stay rejected until it
exists.

## 12. Compatibility and rollout discipline

- **Legacy HELLO is never touched** (no trailing fields, ever).
  `CAP_MULTI_DEVICE = 1 << 2` rides HELLO2 frame 0x06 via
  `core_own_capabilities()` (`core/src/protocol.rs:336`, next free bit
  after `CAP_ACKS_HIDDEN_KINDS = 1`, `CAP_RELAY_UPDATE = 1 << 1`).
- Envelope public header unchanged; new fields sealed-body only (§5).
- **Friend card v4 (`CMFRIEND4`)** adds the roster head (or its hash) so
  new friendships start multi-device-aware. Rollout copies the proven v3
  pattern exactly (`core/src/identity.rs`: parser + emit-gate
  `EMIT_FRIEND_LINK_V3`, `specs/friend-card-v3.md` §Rollout): parser
  ships first on all platforms, emit flips only after the fleet parses
  v4. v1–v3 cards parse forever as a synthetic one-device person whose
  roster arrives later by gossip.
- Legacy peers see: one contact, person-addressed rows (which
  multi-device fleets never ack, ACK-MD-2), envelopes indistinguishable
  from today. A legacy build never needs to know rosters exist.

## 13. Delivery plan (work packages, core-first, riskiest first)

Ground rules for every WP: shared behavior in the Rust core exported via
UniFFI, never per-platform; Android + iOS ship in the same wave; user
copy in `strings.xml` / `Localizable.xcstrings`; `cargo fmt --all` before
committing Rust. Each WP lands as its own PR(s) off `master` in a
dedicated worktree.

**WPT — Forward-tolerance slice (ships FIRST, before wide release,
independent of everything below).** The rest of this spec can land on its
own timeline precisely because deployed builds tolerate what they don't
yet implement — this WP makes that tolerance real on today's fleet
instead of assumed:

1. **Sealed-body tolerance.** Verify (with pinned tests) that the open
   path skips unknown trailing/optional sealed-body fields rather than
   rejecting the envelope; fix it if not. §5 depends on this being true
   of every build in the field.
2. **Capability-bit tolerance.** Verify unknown HELLO2 frame-0x06 cap
   bits are ignored, and that an unknown deep-link/friend-card version
   fails soft (clear "update the app" copy, no crash, no half-parsed
   contact).
3. **Clone guard.** Detect two live devices presenting the same identity
   (the §1 `.cmbak`-clone failure mode) and surface a safety warning
   instead of silent stream quarantine. This protects the entire window
   before WP3's real linking exists.
4. **`CMFRIEND4` parser** (parse-only, emit stays v2/v3 per §12) — ships
   early because rollout lag is fleet-update time, not code time.

*Gate:* tolerance tests pinned in core so a future regression is a
deliberate edit; clone guard exercised in a two-device test; all four
items released to both platforms before the wide-release cut.

**WP0 — Contract + vectors.** This spec merged; executable test vectors
in core for: roster monotonicity/fork/rollback (DL-1/DL-2), stale-roster
sealing, legacy-stream mapping, ACK-MD-2 mixed-fleet cases.
*Gate:* vectors compile and fail against today's behavior where expected.

**WP1 — Core identity split.** Person/Device types, device certs,
domain-separated signing, roster type + DL rules; store migrations
(`contact_devices` table, `sender_device_id` column, epochs); open-path
accepts legacy envelopes forever via `LEGACY_DEVICE_ID`.
*Gate:* full existing core + Android + iOS test suites green with every
peer presenting as a synthetic one-device person; new DL vector tests
green.

**WP2 — Ack + fan-out generalization.** `device_fanout_msg_id`,
per-device hint namespace, ACK-MD rules through
`core_should_ack_inbound` and the fetch/ack planner, own-device rows in
outbound relay fan-out, own-device hints in the fetch hint set.
*Gate (mesh-sim):* two-device recipient over relay — first fetcher must
not starve the sibling; legacy person-addressed row never acked by a
multi-device fleet; BLE-only day converges via §8 once WP4 lands (sim
stub until then). Hint-budget test: max-device fleet stays under
`MAX_FETCH_HINTS`.

**WP3 — Linking.** `CMLINK1` deep link, Noise + SAS ceremony, canonical
bootstrap format + import, two-phase activation, "Replace vs Link"
restore UX (blocked on §14.2 for the recovery-code flow).
*Gate:* link two dev builds end-to-end on LAN and on relay-only; new
device provably silent pre-activation; QR contains no identity secret
(test asserts on payload).

**WP4 — Self-sync.** Sync record kinds, per-device author streams in
authoring paths, digest anti-entropy, SYNC-1..3; convergence property
tests (history, read state, contacts, outbound dedup).
*Gate:* property test — two devices, arbitrary interleaved
online/offline schedules, converge to identical stores; no
double-authoring.

**WP5 — Revocation.** Tombstones, inbox + relay-token rotation, refusal
of revoked signatures, contact notification surface.
*Gate:* mixed-fleet test incl. a months-offline contact sealing to a
stale roster; revoked device demonstrably loses relay fetch after
rotation.

**WP6 — Shell UX (Android + iOS, same wave).** "Your devices" list,
link/remove flows, person-level receipts, Advanced holds per-device
detail. Surface stays family-obvious; capability behind Advanced.
*Gate:* localization CI green; two-phone smoke script extended with a
link + converge pass.

**WP7 — Windows as a linked device.** The desktop links as a normal
device via WP3 (this supersedes a separate helper-identity); LAN
self-sync first, relay rows second.
*Gate:* phone↔desktop link + history convergence on LAN with no relay.

**WP8 — Deferred tail.** Flip `CMFRIEND4` emit after fleet-wide parser
coverage (v3-pattern tripwire test); relay v2 per-device capabilities
when relayd gets its next scheduled pass.

Dependencies: WPT stands alone and ships before wide release; then
WP0 → WP1 → WP2 → {WP3, WP4} → WP5 → WP6 → WP7; WP8 last. WP3 and WP4
can proceed in parallel worktrees after WP2. Nothing after WPT blocks
the wide-release cut.

## 14. Open decisions (David) — gate before the marked WP

1. **Sealing scope (§6) — gates WP2.** Accept the person-scoped inbox key
   on BLE/carry (one copy + self-sync) as specified? The alternative —
   strict per-device sealing everywhere — keeps the plan structure but
   changes §6, §7 costs, and the hint/duty-cycle budgets.
2. **Recovery-code UX (§3, §9) — gates WP3 ship.** The recovery code at
   identity-upgrade time is the one new user-facing burden. Needs a
   family-grade flow (wording, where it's shown, re-display policy)
   before linking ships.
3. **Device cap (§6, §7) — gates WP2.** Proposed soft 8 / hard 16 per
   person, sized by the hint budget (~28 routing identities under
   `MAX_FETCH_HINTS`) and relay fan-out width. Confirm or adjust.
