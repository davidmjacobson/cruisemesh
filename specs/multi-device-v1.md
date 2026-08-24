# Multi-device support v1

Status: **accepted 2026-08-16 — the three §14 decisions are resolved (David,
2026-08-16 interview); ready to implement in the work-package order of §13.**
Ship gate: WPT–WP6 (phones complete) gate production-store promotion; WP7
(Windows) lands after production. Derived from the 2026-08-11 design review
(working notes in `specs/multi-device.md`).

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
- the **recovery material**: per the §14.2 decision, this is the person
  root secret kept only inside the passphrase-encrypted `.cmbak` backup —
  there is no separate memorized code. Restoring/opening the backup can
  sign a roster at a **higher recovery epoch**, which always supersedes
  anything the approving device signed — this is how a stolen approving
  device is dethroned. Consequence: the identity-upgrade flow must nudge
  backup creation (no backup = no override path; the fallback is new
  identity + re-friend, per §2 non-goals).

The person root secret is kept only inside the encrypted backup after
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
  auto-resolve a fork. *Refinement (WP5):* a roster signed by the **person
  root itself** at a strictly higher `recovery_epoch` is heard through the
  quarantine and clears it — that is the one signature a thief provably
  cannot produce, so honouring it is the person resolving the fork rather
  than arithmetic resolving it, and refusing it would leave an
  attacker-caused fork permanently unrecoverable. Everything else stays
  quarantined until a person clears it after out-of-band re-verification
  (`MessageStore::clear_roster_quarantine`).
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
  (`core/src/protocol.rs:1400`, spec'd in
  `specs/group-relay-durability.md` §4.1).
- One relay row per recipient device, each with that device's daily
  recipient hint (per-device hint namespace derived from
  `(person_id, device_id)`); the sender's *own* sibling devices get rows
  too, so relay-reachable siblings converge without a separate channel.
- Each row has exactly **one true consumer**, so the existing consumed-ack
  rule generalizes cleanly through `core_should_ack_inbound`
  (`core/src/engine.rs:378`) and the hint-aware ack planner
  `core_relay_ack_ids_with_consumed` (`core/src/engine.rs:985`), which is
  what every production caller uses and where legacy shared-hint
  withholding already lives.

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
   export (identity material incl. inbox key, **the person's own profile:
   display name, photo, photo revision**, contacts + their rosters,
   group state, recent history head) — NOT a raw sqlite clone; the rest
   of history arrives as ordinary self-sync catch-up. The profile is
   inside the export's signature like everything else, and it is there
   for the reason the closing paragraph of this section gives: restore
   and link are two doors out of the same first-run screen, restore has
   always carried a name and a photo, and landing somebody in two
   different states depending on which door they used is not a choice
   worth having. Without it the adopted phone asks a person their own
   name and stores whatever they type locally, which forks the profile
   against the rest of their fleet.
4. **Two-phase activation.** The approving device signs the new roster
   (seq+1) including the new cert. The new device may not advertise,
   author, or ack ANYTHING until it (a) has imported the bootstrap and
   (b) has acknowledged the exact new roster hash back to the approving
   device. Until then it is invisible on the mesh.
5. Roster gossips to contacts per §4; senders start adding a relay row
   and (eventually) hints for the new device as the roster reaches them.
6. **The adopted device enters the app.** It never re-runs first-run
   setup, and it is never offered the link door it just came through.
   Outstanding permission grants are handled exactly the way a restored
   device handles them — on the home surface, not by putting a wizard
   back in front of somebody whose phone is already linked.

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
5. **The removed device converges and ejects itself.** Steps 1–4 are
   addressed to contacts and to *remaining* own devices, so nothing in
   them ever reaches the device being removed — deliberately, since a
   thief must not be tipped off. But an honest "I found my old phone in a
   drawer" device is on the same footing and otherwise believes itself
   linked forever, keeps advertising, and keeps accepting messages. So:
   when a removed device next meets a sibling on a link that has already
   proved it belongs to this person — a LAN Noise session whose remote
   static key is this person's own agreement key — the sibling pushes the
   current signed roster (frame `0x07`, gated by `CAP_OWN_ROSTER_NOTICE`
   on HELLO2). A device that reads a roster tombstoning itself stores it,
   clears its fleet projection, stops advertising, authoring and acking,
   and surfaces that it was removed.

   It hears only from a document signed under the person root that
   strictly supersedes the one it held, so "you're out" is never a bare
   hint a stranger can inject or a fork can fake. What it cannot race is
   step 1: the inbox key rotates at the moment of removal, before any
   meeting, so the fleet's self-sync channel and retained backlog are
   already shut when the notice arrives.

   **The push is level-triggered, not edge-triggered — the meeting is
   not the only moment it may happen.** The first build made the offer at
   the instant a HELLO2 landed on an own-device link and at no other, and
   a 2026-08-24 two-phone capture showed what that costs: the two phones
   had already met, the removal happened seconds later on a link that was
   already up, and there was no second HELLO to carry it. The removed
   phone held the roster that still listed it through 26 minutes on the
   same Wi-Fi, a force-stop of both apps, and a reboot. So a live
   own-device link is re-offered the current roster on a timer
   (`core_own_roster_notice_reoffer_due`, one minute) for as long as it
   lasts. That is safe to do bluntly because the frame is idempotent in
   both directions — the sender rebuilds it from its store, and the
   receiver refuses anything that does not strictly supersede what it
   holds — and it is deliberately a timer rather than a roster-changed
   event, because the mechanism has to work on the phone that is *wrong*
   and must not depend on any event having been delivered anywhere.

   **And something has to go looking for the link in the first place.**
   A device of this person's own shares their user id, so it has no
   contact row: the LAN transport's automatic subnet sweep was motivated
   only by unlinked *contacts*, and a live own-device link counted toward
   the "this phone has company" test that suppresses the sweep — while
   being, uniquely, the one kind of link no heartbeat ever probed, so a
   half-open one could suppress it for a whole Wi-Fi join. Both are
   corrected: own-device links are heartbeated like any other LAN link
   and closed when they stop answering, they no longer count as company,
   and a roster device this phone has no own-device link to is a sweep
   motive in its own right (`core_lan_scan_gate_open`).

   **Step 2 lands on its own clock, and the notice must not be described
   as though the two were simultaneous.** Both shells drive the relay
   `family_token` rotation now: the removal writes the rotation journal
   as it commits (`begin_relay_rotation`), and the relay sync pass makes
   the call and commits it (`commit_relay_rotation`) the next time this
   phone can reach the family relay. A removal with no internet is still
   a removal — it does not block, fail, or wait — so between the removal
   and that first reachable pass the removed device keeps a working
   family relay credential, and a step 5 meeting inside that window
   (same Wi-Fi, no internet: the ship) tells the holder of that phone the
   exact moment they were removed while the credential is still live. It
   grants no new capability: the token was in that phone's hands the
   whole time, and burning it needs no invitation. So "removed" means cut
   off from the fleet's own traffic *at once*, and cut off from the relay
   mailbox *as soon as the rotation lands* — and nothing (spec, doc
   comment or confirm copy) may collapse the two, in either direction.

   Two costs of step 2 are paid by people who did nothing wrong, and the
   confirm copy says so rather than discovering them in the field:

   - **A sibling that was not there is locked out of the mailbox.** The
     replacement member token reaches the person's other devices over
     §8's Settings stream, which still has no shell transport (the same
     gap step 5's own note names below). Until it has one, a device that
     was not the one performing the removal keeps the retired credential
     and must be handed a fresh `CMRELAY1` setup card from the Shore Pass
     screen. On the common fleet — two devices, one of them the one being
     removed — nobody is stranded, because the only survivor is the phone
     that rotated.
   - **Contacts are repaired by propagation.** Their friend cards carry
     the *deposit* attenuation of the retired token, which dies with it.
     The rotation bumps this device's relay epoch, so the shipped
     `CAP_RELAY_UPDATE` notice fans the new deposit token out on the same
     pass; a contact who is offline for months posts into a 401 until it
     reaches them, and two authoritative rejections mark the endpoint
     stale so nothing loops on it. Anyone who was given the *member*
     token — the other people on a shared Shore Pass — is not repaired by
     that notice at all, because it can only ever carry a deposit
     credential; their repair is a re-shared setup card too.
   - **Two relays refuse the re-key outright, and the app says so.** A
     family whose token comes from an operator's static allowlist answers
     `rotation_unsupported`, and a family whose rotation authority is
     already registered to somebody else's person root — every household
     after the first, on a shared pass — answers `rotation_unauthorized`.
     Neither can ever succeed from this device, so the driver stops
     asking; but the confirmation had already promised the removed phone
     loses the mailbox, so stopping silently would leave that promise
     standing and wrong. Both shells therefore record the refusal and say
     it on Your devices: the pass could not be changed, the removed
     device can still reach the mailbox, and the repair is a new pass. On
     a **shared** pass the first household to remove a device also takes
     the mailbox away from the other holders of that member token, who
     have no in-app repair either; nothing gates the rotation on the pass
     being unshared, because no device can tell one from the other.

Notes on step 3, recorded so the guarantee is not read as stronger than
it is (WP5):

- **The device id it refuses is not yet device-signed.** §5's authoring
  signature is WP1's and unbuilt, so `sender_device_id` is a label
  authenticated as coming from the sender rather than a signature by the
  device it names. Step 3 therefore stops a revoked device speaking under
  its own name, and does not stop whoever holds that install's identity
  key from relabelling. The capability half of §10 — the inbox key and
  the relay token — is what does not depend on this. Prerequisite filed
  as `MD-AUTHORING-DEVICE-SIGNED`.
- **Arrival, not authorship, is what is judged.** A body the device
  wrote before its revocation and that arrives after it — an ordinary DTN
  carry or a relay row that sat for days — is dropped, because the only
  "when" available is one the sender chose and believing it would let a
  thief backdate everything.
- **Step 2's rotation is authorized by the person root, not by the
  member token**, since the revoked device holds that token. relayd
  registers the rotation key on first use and pins it after; the
  consequence is that a shared Shore Pass becomes rotatable by exactly
  one person, which matches the organizer reality.
- **Step 1 does not rotate the key contacts write to.** Inbox key
  generation 0 *is* the deployed person agreement key that every friend
  card carries, so rotating it would mean re-friending everybody — a §2
  non-goal. What the rotation withdraws is the fleet's self-sync channel,
  the retained backlog, and (with step 2) the relay mailbox.

Notes on step 5, for the same reason:

- **A BLE-only meeting does not converge.** BLE HELLO is cleartext and
  proves nothing about who is on the other end, so it cannot satisfy the
  test the notice requires and never carries one. That limitation is
  recorded here rather than fixed by weakening the test: the roster is a
  private fact about how many devices a person has, and a stranger who
  claimed this person's `user_id` in a HELLO must not be able to elicit
  it. Same Wi-Fi is the common family case and is what converges.
- **A quarantined fork shadows ejection.** A removed device handed a
  document that forks the roster it holds (DL-2) quarantines instead of
  ejecting, and stays live. That fails toward not bricking a device on
  the strength of one branch of a fork — the alternative would make a
  fork weaponisable into a remote stop — and it leaves the reported
  symptom intact in that corner. The shell surfaces the quarantine rather
  than swallowing it.
- **The notice carries no key material.** It is a plaintext link frame
  carrying the DL-3 document and nothing else, so a device that is still
  listed but whose roster announces a *rotated* inbox key generation does
  not adopt it — there would be no key to open the fleet's traffic with.
  Step 1's sealed handoff is what carries that; the notice reports the
  gap instead of half-applying it.

  Read the consequence plainly: **step 5 converges the removed device,
  not the rest of the fleet.** Every removal mints a new inbox key, so
  every removal roster announces a rotated generation, so a *sibling*
  that was offline when the removal happened hits exactly this arm and
  keeps the pre-revocation roster — and with it the removed device's
  `device_id` in its own fleet projection — until §10.1's sealed handoff
  reaches it. That handoff has no shell transport yet (it rides
  self-sync), so today it does not reach it at all. WP5's gate is
  written against the removed device for that reason, and a fleet-wide
  convergence claim would be false.
- **The stage is terminal.** A device that has ejected itself is not in a
  window that closes: DL-4 buries its `device_id` forever, so the only
  way back is a fresh install under a fresh device key. The §9.4 gate
  gains one stage for this, and neither beginning nor abandoning a
  ceremony can leave it.

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
  `core_own_capabilities()` (`core/src/protocol.rs:355`; the constant is
  already reserved at `core/src/protocol.rs:350`, next free bit after
  `CAP_ACKS_HIDDEN_KINDS = 1`, `CAP_RELAY_UPDATE = 1 << 1` — WPT shipped
  the reservation, WP1 flips the advertisement).
- `CAP_ROSTER_GOSSIP = 1 << 3` (kind 21, DL-3) and
  `CAP_OWN_ROSTER_NOTICE = 1 << 4` (frame `0x07`, §10 step 5) ride the
  same frame under the same rule: one bit per thing a peer must
  understand, never a bit that quietly grows a member, so an
  advertisement a shipped build made honestly keeps meaning what it meant.
  A build that predates frame `0x07` refuses the unknown type byte in
  `parse_frame` and both shells drop an unparseable frame without
  touching the link, so sending it is safe even where it is pointless.
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
rotation; a revoked device that meets its approver on a LAN self-ejects
— stops advertising, authoring and acking, and stops reporting a fleet
it is not in.

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

## 14. Decisions — resolved by David, 2026-08-16

1. **Sealing scope (§6): RESOLVED — person-scoped inbox key on BLE/carry,
   as specified.** Strict per-device sealing everywhere stays the
   documented escalation if the threat model hardens. WP2 unblocked.
2. **Recovery UX (§3, §9): RESOLVED — recovery lives in the backup.** No
   separate memorized code: the person-root secret exists only inside the
   passphrase-encrypted `.cmbak`; the recovery flow is "open your
   backup." The upgrade flow nudges backup creation, and the Before-you-
   sail checklist's backup item doubles as the standing nudge. No backup
   ever made = no override path (new identity + re-friend fallback).
   WP3 unblocked.
3. **Device cap (§6, §7): RESOLVED — soft 8 / hard 16 per person**, as
   proposed. Boundary semantics (pinned by the WP0 vectors): a person may
   hold up to 16 devices; adding a device that would make the count exceed
   8 succeeds with a warning (the 9th device warns, the 8th does not); an
   add that would exceed 16 is refused (the 17th device). WP2 unblocked.

Ship-gate decision (same interview): **WPT–WP6 gate production-store
promotion; WP7 (Windows as linked device) does not** — it lands after
production on its own timeline. WP8 unchanged (deferred tail).
