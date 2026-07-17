# DTN Hardening: Audit Re-Assessment & Sub-Agent TODOs

Successor to `DTN_AUDIT.md` (audited master `68abbe1`), re-verified against
the Rust-migration branch `claude/rust-migration-audit-todos-a7lly6`
(`8a12e17`, "Move shared mobile protocol logic into Rust core"). Companion to
`RUST_MIGRATION_TODOS.md`, which this file assumes has merged: **every task
below baselines on the migration branch (or master after it merges), not on
`68abbe1`.** Format follows RUST_MIGRATION_TODOS.md: each TODO owns its
files exclusively so independent tasks can go to separate agents without
merge collisions; behavioral decisions are recorded up front so agents don't
re-litigate them.

## 1. What the migration changed for the DTN audit

The migration moved the DTN-critical decisions into `core/src/engine.rs`
(inbound gate, disposition-gated relay acks, digest spray plan),
`transport_policy.rs`, and `relay_wire.rs`. Findings re-verified
one-by-one:

| Audit finding | Status on migration branch |
|---|---|
| F1 relay poll-only (60 s, no WS push) | **Still open.** No WS client on either platform; relayd's push hub still unused. |
| F2 mule deletes carried copy on dispatch | **Still open, now on BOTH platforms** — `MeshService.drainCarriedEnvelopesTo` (`removeCarriedEnvelope` after fire-and-forget send) and the same pattern in `MeshController.drainCarriedEnvelopesTo`. Digest still advertises carried-only msg_ids. |
| F3 consumed-elsewhere relay copies never acked | **Still open, now enshrined in core** — `engine.rs::core_should_ack_inbound` acks only Consumed/Expired; SEEN is never acked, so BLE-delivered envelopes are re-fetched every 60 s pass until restart/30-day expiry. |
| F4 seen-set poisoning on carry failure | **Still open** — native code calls `seenIds.checkAndRecord` *before* `core_inbound_gate`; a later store failure leaves the id poisoned. |
| F5 no in-session re-digest on quiet links | **Still open.** Digest exchange remains connect-time only. |
| F6 7-day expiry is a silent delivery deadline | **Still open** (UI/visibility gap). |
| F7 group lamport-0 resend + local-only group receipts | **Still open** — `resendGroupOutboundToPeer` unchanged. |
| F8 wall-clock dependence | Still open, still low priority. |
| F9 LAN validation gates unproven | Still open (testing, not code). |
| F10 iOS background ceiling | Unchanged, but the migration **fixed the iOS-weakening drift**: 16-bit framing, disposition-gated acks, lamport ratcheting, and full digest spray now shared. iOS mule parity is real now. |

New (from reviewing the migration itself and relayd):

| New finding | Status |
|---|---|
| N1 group relay envelopes acked on first CONSUMED fetch | Pre-existing on master, **carried into `engine.rs`**: the family shares one mailbox per token, so the first member to fetch+consume a group envelope deletes it for every other member. Needs a design decision, not a quick patch. |
| N2 relayd has no per-envelope size cap or per-family storage quota | Only axum's default request-body limit stands between a client and unbounded SQLite growth. |

### Fate of the three pre-migration fix branches (2026-07-16)

- `agent/relay-ws-push` (F1): **merges cleanly** onto the migration branch —
  keep it; D3 below adds the iOS half.
- `agent/mule-drain-confirm` (F2): conflicts (MeshService rewrite).
  **Superseded by D2** — reimplement in core; port its Rust store method +
  selector tests, which remain valid references.
- `agent/relay-ack-seen-consumed` (F3): conflicts (modify/delete on
  `RelayInbound.kt`, now `engine.rs`). **Superseded by D1** — same rule, new
  home; port its `MessageOrigin` query + the group-row test cases.

## 2. Ground rules for every TODO

- Baseline: `claude/rust-migration-audit-todos-a7lly6` (or master once it
  merges). Do NOT base on `68abbe1` — you will reimplement deleted files.
- DTN decision logic goes in `core/src/` (usually `engine.rs` or
  `store.rs`), exported via UniFFI; both platforms call it. Never fix a DTN
  behavior in one platform's shell.
- Native keeps: radios, sockets/HTTP/WS execution, lifecycle, UI.
- Tests: Rust logic gets Rust tests (`cargo test -p cruisemesh-core`);
  platform seams keep thin platform tests. Android validation per AGENTS.md
  (bindgen + `testDebugUnitTest`). iOS validation runs on the Mac host per
  AGENTS.md ("macOS Validation Host") — treat `~/cruisemesh` as user-owned,
  work in a sibling worktree.
- Each TODO owns its listed files exclusively. D2 and D3 both touch
  `MeshService.kt`/`MeshController.swift` — run them in different waves (§4).

## 3. Behavioral decisions (recorded; do not re-litigate)

1. **Relay ack safety:** never ack a relay copy unless THIS device was the
   envelope's sole true endpoint consumer. Concretely: ack a SEEN envelope
   iff the local messages table has a row for its `msg_id` with
   `sender_user_id != own` AND `chat_id == sender_user_id` (the 1:1
   incoming-row convention). Group rows (`chat_id == group.id`) are never
   acked from the SEEN path — the shared family mailbox is every member's
   copy. Hidden kinds (no msg_id row) are never acked. When in doubt, don't
   ack: churn is recoverable, deletion is not.
2. **Mule copy lifetime:** a carried 1:1 envelope is removed only on
   digest-proof of receipt (peer advertises the msg_id), never on dispatch.
   Worst case is a duplicate resend (deduped by the peer), never loss;
   expiry still bounds queue growth. Recipients must therefore advertise
   recently *held* message-stream msg_ids (consumed incoming + own authored)
   in their DIGEST alongside carried ids, under the existing 512-entry cap.
   No wire change — the DIGEST recent-id list already has arbitrary content.
3. **WS push is a doorbell only:** any pushed frame triggers the existing
   authoritative poll/fetch/ack pass. The push path never parses, stores, or
   acks envelopes. Poll stays as fallback.
4. **Group relay durability (N1)** requires a design round before code —
   candidate directions in D6; an agent picking up D6 produces a short spec
   first, not a patch.

## 4. Dependency order / parallelism

```
Wave 1 (parallel): D1 (engine ack rule)   D4 (seen-set ordering)   D7 (relayd limits)
Wave 2 (after D1): D2 (mule confirm — owns MeshService/MeshController wiring)
Wave 3 (after D2): D3 (WS push — merge Android branch, add iOS client; touches the same two shell files)
Anytime, independent: D5 (expiry UI), D8 (design: group durability spec, then implement)
Later / larger: D9 (per-group digests), D10 (periodic re-digest — small, but touches the shells; slot into wave 3+)
Human/device tasks, not agent-codeable: V1 (LAN gates), V2 (field metrics)
```

## 5. TODO list

### D1 — Consumed-SEEN relay acks in the core engine *(was F3; port of `agent/relay-ack-seen-consumed`)*

`engine.rs` acks only Consumed/Expired. Add the consumed-SEEN rule per
decision §3.1: a store query `message_origin_by_msg_id(msg_id) ->
Option<MessageOrigin { chat_id, sender_user_id }>` (add
`idx_messages_msg_id`), and either extend `core_relay_ack_ids` to take the
origin lookup or add a `MessageStore` method
`core_relay_ack_ids_with_consumed(...)` so both platforms get it for free.
Port the old branch's Rust tests (1:1 row ackable; group row, own-authored
row, unknown id, hidden-kind all not ackable). Update both shells' poll
paths to pass msg_ids through. KDoc/doc-comment the §3.1 invariant verbatim,
and note N1 (CONSUMED group ack) as out of scope.

**Owns:** `core/src/engine.rs`, `core/src/store.rs` (additive query +
tests), `core/src/lib.rs` exports; the relay-ack call-site lines in
`android/.../mesh/MeshService.kt` `pollRelayMailbox` and
`ios/CruiseMesh/Mesh/MeshController.swift` relay poll (call-site lines only
— wave 1 runs before D2's rewrite of those functions' neighbors).

**Acceptance:** cargo tests for all five rule cases; Android suite green;
iOS suite green on the Mac host; a BLE-consumed 1:1 envelope's relay copy is
acked on the next poll pass, a group envelope's is not.

### D2 — Confirm-before-delete muling in the core *(was F2; port of `agent/mule-drain-confirm`)*

Per decision §3.2. Core side: port `recent_consumed_msg_ids(limit)` from the
old branch (docs corrected: it returns consumed incoming AND own-authored
ids — both useful to advertise); add a digest-advertised-ids builder
(carried first, then message-stream ids, 512 cap) so both platforms build
the same list; add `core_confirm_carried_deliveries(peer_user_id,
peer_known_msg_ids, now_ms)` on `MessageStore` that removes carried
envelopes matching the peer's own recent-day hints whose msg_id the peer
advertised (never group-hint carries — separate lifecycle). Shell side
(both platforms): `drainCarriedEnvelopesTo` stops calling
`removeCarriedEnvelope`; digest handler calls the confirm method with the
peer's advertised ids; digest send uses the new builder.

**Owns:** `core/src/store.rs` (additive), `core/src/engine.rs` (confirm +
builder if placed there), `android/.../mesh/MeshService.kt`,
`ios/CruiseMesh/Mesh/MeshController.swift` (drain/digest functions).

**Acceptance:** Rust tests incl. the old branch's selector cases; both
platform suites green; a mid-transfer BLE drop no longer loses the mule's
only copy (mesh_sim scenario if feasible: kill link between dispatch and
receipt, verify redelivery on reconnect).

### D3 — Relay WS push, both platforms *(was F1)*

Android: merge `agent/relay-ws-push` (verified clean merge onto the
migration branch) — re-run its tests, reconcile imports if D2 landed first.
iOS: implement the same doorbell with `URLSessionWebSocketTask` in
`ios/CruiseMesh/Relay/` (connect to `GET /ws?hints=...` with Bearer auth
when the app is foregrounded with internet; reconnect with the ported
backoff policy from `transport_policy.rs` or a small Swift mirror of
`RelayPushBackoff`; any received frame → the existing relay sync trigger).
Honor decision §3.3. Document the iOS limitation: WS lives only while the
app has execution time; poll remains authoritative.

**Owns:** `android/.../relay/RelayPushClient.kt`, `RelayPushBackoff.kt` (+
tests) via the branch merge; `ios/CruiseMesh/Relay/RelayPushClient.swift`
(new) + test; the wiring lines in both shells' service/controller.

**Acceptance:** two internet-connected devices see sub-5-second relay
delivery (manual/integration check); WS drop degrades silently to the 60 s
poll; no envelope processing anywhere in the push path.

### D4 — Fix seen-set poisoning ordering *(was F4)*

`checkAndRecord` currently fires before the envelope is durably handled; a
store failure (disk full) poisons the msg_id for the process lifetime and
every future copy is dropped as SEEN. Restructure to check-then-record:
native (or a new core gate wrapper) records the id only after the envelope
was consumed, carried, or deliberately dropped (expired/relayed-only).
Keep the outgoing-path `record` calls as-is.

**Owns:** `core/src/gossip.rs` (add a non-recording `contains` if missing),
`core/src/engine.rs` (`core_inbound_gate` signature if it absorbs the
record step), the inbound-envelope entry points in both shells.

**Acceptance:** Rust test: gate → simulated store failure → same msg_id
re-presented → Dispatch (not Seen). Both platform suites green.

### D5 — Expiry visibility in the UI *(was F6)*

A message that expires undelivered (7-day `DEFAULT_EXPIRY_MS`) silently
dies; ticks stay at ✓ forever. Add a semantic query in core (message is
past-expiry AND below the peer's delivered watermark → `ExpiredUndelivered`
tick state via `semantic.rs`/`TickStatus`), and render it distinctly with
honest copy ("couldn't be delivered — expired"). Do not change expiry
mechanics. Optional follow-up noted, not owned here: author-side re-stamp
("retry") that re-authors the payload under a fresh envelope.

**Owns:** `core/src/semantic.rs` (+ store query), both platforms'
`TickStatus` + chat-row rendering touchpoints.

### D6 — Group relay durability design *(N1; design-first)*

Problem: one family token = one mailbox; first member to fetch+consume a
group envelope acks it away from everyone else (engine keeps master's
behavior). Deliverable: a short spec (`specs/group-relay-durability.md`)
weighing at least: (a) never ack group-addressed envelopes (30-day
retention + expiry bounds growth; cheapest), (b) per-member ack tracking
server-side (relayd schema change), (c) member-count-aware acks. Include
migration/compat notes for old clients. Implement only after the user
approves the spec.

**Owns (spec phase):** `specs/group-relay-durability.md` only.

### D7 — relayd resource limits *(N2)*

Add an explicit per-envelope sealed-size cap (reject oversized POSTs with a
clear error; pick the cap from the attachment ceiling + margin) and a
per-family-token storage quota with oldest-first eviction (mirroring the
client's foreign-carry budget philosophy) or a documented refusal — decide
which and document it in `relayd/DEPLOY.md`. Keep prune-on-fetch.

**Owns:** `relayd/src/lib.rs`, `relayd/tests/`, `relayd/DEPLOY.md`.

### D8 — Periodic re-digest on long-lived links *(was F5)*

A frame lost mid-session (unacked BLE notification, reassembler drop) waits
for the next reconnect/send/relay pass. Add a re-digest interval decision to
`transport_policy.rs` (e.g. every 3–5 min on a live link, jittered), driven
by the shells' existing timers; re-running the digest exchange is already
idempotent. LAN links may piggyback on the probe cadence.

**Owns:** `core/src/transport_policy.rs` (+ tests), timer wiring lines in
both shells.

### D9 — Per-group digests + wire group receipts *(was F7; largest)*

Replace `resendGroupOutboundToPeer`'s lamport-0 full resend with per-group
digest entries (the DIGEST frame's chat_id field already supports it —
send one digest frame per shared group), and put group delivered/read
receipts on the wire (per-member cumulative receipts, aggregated to the
"✓✓ = all members" UI both READMEs already promise). Protocol-compatible
path: old clients ignore unknown digest chat_ids and keep the resend
behavior. Needs its own mini-spec section in the PR description; core-first
(store queries + engine plan), shells wire it.

**Owns:** `core/src/store.rs`/`engine.rs`/`protocol.rs` (additive),
digest/group functions in both shells, group tick UI touchpoints.

### V1 — LAN transport validation gates *(was F9; human + devices)*

Run `specs/same-lan-transport.md` §Validation gates on real networks
(permissive LAN, client-isolated LAN, captive portal; A↔A, i↔i, A↔i;
screen-off; roaming). Record results in the spec. Agents can help by adding
diagnostics surfaces, not by running it.

### V2 — Field metrics for the cruise test *(supports the whole audit)*

Log delivery latency + mode mix (direct BLE / LAN / mule / relay / push) per
message locally (Message Info already stores arrival transport + hops), and
add an export/summary screen so the DESIGN.md §11 M5 field test produces
data. Small, self-contained, both platforms.

## 6. Standing notes for the orchestrator

- Findings F8 (clock skew) and F10 (platform ceilings) remain accepted
  risks; revisit F8 only if field data shows hint misses.
- `DTN_AUDIT.md` (2026-07-16) has the full original analysis and per-file
  evidence; this file supersedes its priority list.
- When dispatching an agent: give it the D-item verbatim, the §2 ground
  rules, the §3 decisions, and the file-ownership list; require pasted test
  output; review the diff against the §3 invariants before merging —
  yesterday's review caught a group-ack safety hole exactly there.
