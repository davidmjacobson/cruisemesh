# Protocol Contract v1

**Status:** normative index, revision 1

**Scope:** the deployed CruiseMesh protocol as it exists today. This document
pins behaviour that is already shipped. It does not propose a protocol v2, a
new frame, or a wire change of any kind.

## How to read this

CruiseMesh runs the same protocol from three places: the Rust core, the
Android shell, and the iOS shell. Whenever a decision is made twice, the two
copies eventually disagree, and every field incident this contract cites
started that way. The fix is not more review — it is naming each rule once,
giving it an id, and pointing that id at a test that fails loudly when the
rule breaks.

So:

- **Tests are normative for transitions.** If the prose here and a listed
  test disagree about what happens on a short page, the test is right and the
  prose is a bug.
- **Prose is normative for security rationale, interoperability, limits, and
  terminology.** A test cannot tell you *why* HELLO is unauthenticated or why
  a carried row may not be deleted on send.
- **Every rule has an id.** The id is printed when its test fails, so a red
  build names the rule that broke rather than a function.
- **Every rule has a named owner.** `core/tests/protocol_contract.rs` is the
  index of record. An invariant with no executable owner is carried there as
  an explicitly ignored, explicitly named marker — never as prose alone.

Ownership honesty matters more than coverage optics. Three owner classes
appear below:

| Owner class | Meaning |
|---|---|
| **core** | A Rust test in `core/` pins the rule. The contract index re-asserts it so the invariant id prints on failure. |
| **hoist-pending** | The real decision still lives in a platform shell, and a named Kotlin/Swift test pins it there. The contract index carries an ignored marker naming both the shell test and the work package that will move it. |
| **unimplemented** | Nothing pins the rule yet. The contract index carries an ignored marker naming the work package that will own it. |

An `UNIMPLEMENTED` marker is not a failure of this document. It is the point
of it: the gap is now countable.

## 1. Invariants

| ID | Rule | Owner class | Executable owner |
|---|---|---|---|
| `ACK-01` | Proxy/carry disposition never makes a relay copy ackable. `SEEN` is ackable only with permitted durable local-consumption proof. | core | `core/src/engine.rs` ack tests; index re-asserts `core_should_ack_inbound` |
| `CARRY-01` | Sending or relay-uploading a carried row does not remove it. Only verified peer digest/receipt proof or expiry permits removal. | core | `core/src/engine.rs` confirm-carried tests, `core/src/store.rs` carry tests; index re-asserts upload-then-survive |
| `CURSOR-01` | A frontier advances only across a fully processed page whose required acks succeeded, and never moves backward on a normal pass. | core | `core/src/relay_cursor.rs` advance tests; index re-asserts `relay_cursor_advance` |
| `PAGE-01` | Only an empty page is EOF. A short page continues. A non-empty non-advancing page terminates safely without unsafe advancement. | core | `core/src/relay_cursor.rs` walk-continuation tests; index re-asserts `relay_fetch_walk_continues` |
| `RATE-01` | The first family 429 ends remaining pass network work. `Retry-After` is a floor, and pending nudges cannot bypass the quiet window. | hoist-pending | floor clamp is core (`relay_retry_after_ms`); pacing, backoff and rerun still shell-owned |
| `ENDPOINT-01` | A phone advertises only its own endpoint. A discovered or third-party address is never forwarded to anyone. | hoist-pending | receive-side scoping is core; hint authoring is shell-owned |
| `SILENCE-01` | Contact silence advances only with same-pass proof that another relay answered; authoritative rejection does not require that proof. | core | `core/src/contact_relay_health.rs` silence tests; index re-asserts the delta rule |
| `UI-01` | Delivery and via-transport claims require persisted arrival or receipt evidence, never a current-link guess. | core | `core/src/connection_health.rs` delivery tests; index re-asserts the queue-honesty gate |
| `LIVE-01` | Every pass terminates inside its declared request, envelope, byte, and time/yield budgets. | unimplemented | package C0 (`CoreRelayPass` + replay runner) |
| `PROGRESS-01` | A continuation must strictly advance a frontier/work cursor or strictly increase a future deadline/backoff. Unchanged-state reschedule loops are forbidden. | unimplemented | package C0; walk-budget half is already core |
| `MARK-01` | A successfully relay-uploaded carried row is durably marked before the pass ends; the marker survives restart and suppresses repeat upload for its lifetime. | core | `core/src/store.rs` upload-marker tests; index re-asserts first-writer-wins |
| `WM-01` | Receipt repair has a reachable, bounded path from every supported stored state; a peer watermark of zero cannot permanently gate repair. | hoist-pending | shell repair planners |
| `SPRAY-01` | Carried-first work toward one peer is bounded per encounter **in bytes as well as in rows**, and re-offers to the same peer are rate-gated, so a large carrier cannot starve receive work or trip an OS watchdog. | hoist-pending | core spends a per-encounter byte budget, but every budget number is a shell constant; the per-second cadence gate is not built at all |
| `HELLO-01` | Legacy HELLO never gains trailing fields; new capabilities use HELLO2 frame `0x06`. | core | `core/src/protocol.rs` HELLO/HELLO2 codec tests; index re-asserts both shapes |
| `IDEMP-01` | Duplicate, late, or replayed external results cannot double-apply a mutation, regress a cursor, or consume a carried row. | unimplemented | package C0 |
| `TXN-01` | No store transaction spans external I/O. Page consume and frontier advancement retain their documented two-transaction crash safety. | unimplemented | package C0 |
| `QUEUE-01` | Proof of delivery for a 1:1 outbound envelope permits — and the queue eventually performs — its retirement, and a payload whose usefulness is shorter than its expiry is superseded rather than re-advertised. The advertised outbound set shrinks under coverage; flat expiry is a backstop, never the only retirement path. | core | `core/src/outbound_retirement.rs` coverage, sweep, supersession and expiry tests (#283); index re-asserts that a delivered watermark shrinks both readers of the queue |
| `SECRET-01` | Events, fixtures, summaries, and exported diagnostics contain no relay tokens, raw friend cards, plaintext, private keys, or full endpoint-bearing bodies. | core | `core/tests/protocol_contract.rs` fixture canary scan |

### 1.1 What each rule means, for someone reading it cold

#### `ACK-01` — never ack mail you did not consume

Acking a relay row deletes the server's copy. That copy is frequently the
only copy. A device may therefore ack a row only when it can prove *it* was
the sealed payload's true endpoint consumer. Relaying the row onward, muling
it for someone else, or merely recognising its id are all disqualifying:
`CARRIED`, `REJECTED` and `FAILED` never ack. `SEEN` — "I already have
this" — acks only when durable local evidence names the same envelope, and
never for a group-addressed row, because a group row is one shared server
copy that other members still need.

#### `CARRY-01` — handing a message on is not proof it arrived

A carried envelope is the mesh's memory. Offering it over Bluetooth, or
uploading it to a relay, proves only that this device did something — not
that anyone received anything. A carried row is removed on exactly two
grounds: a peer's digest or receipt naming the envelope, or expiry. This is
the rule that makes an intermittently-connected fleet converge rather than
silently lose mail at the first flaky link.

#### `CURSOR-01` — the frontier only moves over ground you actually covered

A mailbox frontier says "everything below this id is handled". It may
advance only across a page that was fully processed and whose required acks
succeeded. A partially processed page holds the frontier where it was. On a
normal pass the frontier never regresses; the single exception is deliberate
and narrow, and it is a *repair*, not a retreat: when a completed sweep
proves the server's id space itself regressed — a rebuilt mailbox — the
frontier is lowered to match the ground truth the sweep observed (#279).

#### `PAGE-01` — an empty page is the end; a short page is not

Servers clamp page sizes, byte budgets truncate pages, and both produce a
page shorter than requested. Treating short as EOF strands every row above
it. Only a genuinely empty page ends the walk. A non-empty page that fails
to advance the cursor is a server or client fault, and the walk terminates
on it rather than looping — it must never respond by advancing anyway.

#### `RATE-01` — one 429 stops the pass, and the quiet window is real

The family relay budget is shared across every phone in a family. The first
`429` on family work ends the remaining network stages of that pass rather
than continuing to spend the shared bucket. `Retry-After` is a floor, not a
suggestion: repeated 429s widen the quiet period, and a nudge that arrives
during the window is deferred into it rather than starting a fresh pass. A
pending rerun that ignores the window turns a rate limit into a hot loop —
the exact shape of the re-upload storm fixed in #222 and the abort fixed in
#260/#261.

#### `ENDPOINT-01` — every phone advertises only itself

A device may publish its own reachability and nothing else. Addresses learned
from a peer, a scan, or a third party's frame are never forwarded onward, and
a relay-change notice may only change the endpoint of the contact who sealed
it. This is a privacy invariant first — a mesh that gossips addresses builds
a map of who is near whom — and it is also the property that stopped a
cross-subnet hint from poisoning the endpoint cache in #271/#278.

#### `SILENCE-01` — you cannot blame a peer for your own dead internet

Resting a contact's endpoint because it "went silent" requires same-pass
proof that some other relay answered. Without that proof the silence is
indistinguishable from this device being offline, and acting on it writes off
a healthy contact. An authoritative rejection — a credential or family
refusal the server actually returned — needs no such proof, because the
server plainly answered.

#### `UI-01` — only evidence may be shown as delivery

A connected socket, a GATT link, a Wi-Fi association, or a live push
subscription is diagnostic state. None of them is delivery. The interface may
claim a message arrived, or claim a transport carried it, only from a
persisted arrival or receipt observation. Where the underlying number cannot
mean delivery — a relay-upload backlog on a phone with no pass, which never
drains and never did mean delivery — the honest answer is to say nothing.

#### `LIVE-01` — every pass ends

A pass declares budgets: requests, envelopes, bytes, and wall time or yields.
It terminates inside them. "Terminates eventually" is not the property; a
pass that runs for minutes has already failed, because on a phone it is
competing with a watchdog.

#### `PROGRESS-01` — a continuation must buy something

Rescheduling is permitted only when the reschedule strictly advances a
frontier or work cursor, or strictly increases a future deadline or backoff.
A continuation that leaves state unchanged is a livelock with good manners,
and it is exactly what emptied batteries before #270.

#### `MARK-01` — upload once, and remember it across a reboot

A carried row that reached a relay is marked durably before the pass ends.
The marker survives process death and suppresses re-upload for the row's
lifetime. Without it, every launch re-posts the whole carry queue — the
re-upload storm that put a real device at hundreds of posts per minute
against its own family's rate bucket (#222). Markers are first-writer-wins,
and they are cleared wholesale only when the destination mailbox itself
changes.

#### `WM-01` — repair must always have a way out

Receipt repair must be reachable and bounded from every stored state the app
supports. In particular a peer watermark of zero — a peer that has never
acknowledged anything — must not become a permanent gate on repair. A
self-locking pairing that can only be fixed by reinstalling is a protocol
bug, not a support case (#241).

#### `SPRAY-01` — a big carrier must not drown a small one

Carried-first work toward one peer is bounded per encounter, and the bound is
in **bytes** as well as in rows. Row counts alone do not bound anything: a
single 18 KB frame repeated 34 times inside one second is 34 rows and
roughly 640 KB queued at one peer, which is what issue #280 records. So the
bound has two parts: a byte budget per encounter, and a cadence gate that
stops the same offer being re-queued while an earlier copy is still in
flight. Neither may starve receive work, and neither may exceed what the
platform's radio callbacks can drain before a watchdog fires.

#### `HELLO-01` — the legacy handshake is frozen

Legacy HELLO (`0x01`) is `frame type ‖ user_id`, where `user_id` is the whole
remainder. It has no length field, so any trailing byte added to it is
indistinguishable from a longer user id, and every deployed build would
mis-parse it. It never gains a field. New capabilities ride HELLO2 (`0x06`),
which is length-defined and explicitly tolerates trailing bytes, and the
advertised bit set comes from `core_own_capabilities()` so the two shells
cannot disagree.

#### `IDEMP-01` — the same answer twice changes nothing

External results arrive duplicated, late, out of order, and after
cancellation. A result that is a duplicate, belongs to a finished pass, or
names an action that is no longer outstanding performs no store mutation. It
cannot double-apply, regress a cursor, or consume a carried row. It emits a
stable diagnostic code and is otherwise inert.

#### `TXN-01` — never hold a transaction across the network

No store transaction spans HTTP, Bluetooth, LAN, a timer, or a platform
callback. Page ingestion and frontier advancement are deliberately two
separate short transactions with the network ack between them, so a crash at
any point replays safely: a consumed-but-unacked page re-presents and the
frontier stays put.

#### `QUEUE-01` — a delivered message must be able to leave the queue

The outbound queue is what this device still owes other people. It must
shrink for the two reasons that actually retire an obligation:

1. **Coverage.** Proof of delivery for a 1:1 outbound envelope — a receipt
   watermark at or above it, or an equivalent durable arrival record —
   permits its retirement, and the queue eventually performs that retirement
   rather than merely knowing about it.
2. **Supersession.** A payload whose usefulness is shorter than its expiry —
   a reachability hint, a directory snapshot, a profile or relay-change
   notice — is replaced by its successor instead of both being advertised.

Flat expiry is a backstop for what neither rule reaches, never the only
retirement path. Issue #283 records the failure mode: a real device holding
3,786 unexpired outbound rows, 2,018 of them already covered by receipts, of
which roughly 93% were service kinds, including week-old reachability hints
whose payload stops being true after about fifteen minutes. The advertised
set must shrink under coverage.

Two boundaries are part of the rule rather than details of one
implementation of it.

**Retirement removes a retransmission artifact, never the ability to
retransmit.** A covered envelope may leave only while the stored message
that regenerates it stays, because the receipt watermark this rule consults
is a MAX over a peer's stream (`WM-01`) and can legitimately sit above a
lamport that peer never filed. With the message kept, a peer that later
reports the hole in its gap-aware digest is served a re-sealed envelope; the
sender's obligation survives the queue row. Carried rows — other people's
mail, of which this device may be the only copy — are outside this rule
entirely and keep leaving only under `CARRY-01`.

**Group rows are excluded, and the group rule is deliberately not stated
here.** A group envelope is queued once against the group id and fanned out
per member, and group wire receipts are deferred, so no single watermark can
mean "every member received it". Retiring one on a group watermark would
drop mail for the members who did not get it. Group retirement needs a
per-member coverage record that does not exist yet; until it does, this rule
governs pairwise 1:1 rows only.

#### `SECRET-01` — diagnostics must be safe to send

Anything that can leave the device — protocol events, replay fixtures, pass
summaries, exported archives — contains no relay token of either class, no
raw friend card, no message plaintext, no private key material, and no
endpoint-bearing body. Redaction may keep length, digest, kind, and a stable
archive-local pseudonym. A test scans serialised records for known canaries
rather than trusting the authoring code.

## 2. Frames, envelopes, kinds, and limits

Derived from `core/src/protocol.rs`, `core/src/framing.rs`, and
`core/src/limits.rs`.

### 2.1 Frame types

Every byte string on the peer link is a frame: one type byte, then a
type-specific body. The link layer's own fragmentation delimits the frame, so
frame bodies carry no length prefix of their own.

| Byte | Frame | Body |
|---|---|---|
| `0x01` | HELLO | `user_id` = the entire remainder. Unauthenticated by design. |
| `0x02` | ENVELOPE | `msg_id`(16) ‖ `hop_ttl`(u8) ‖ `expiry`(i64 BE) ‖ `recipient_hint`(8) ‖ sealed payload |
| `0x03` | DIGEST | `chat_id` ‖ per-sender `(sender_user_id, through_lamport)` entries ‖ recent `msg_id` list |
| `0x04` | LAN_ENDPOINT | version(1) ‖ instance token(8) ‖ port(u16) ‖ host length(u8) ‖ host |
| `0x05` | TRANSPORT_PROBE | version(1) ‖ response flag(0 or 1) ‖ nonce(u64) |
| `0x06` | HELLO2 | `user_id`(16) ‖ `capabilities`(u32 LE) ‖ trailing bytes tolerated |

**Unknown frame type:** rejected as malformed. There is no forward-compatible
"skip unknown frame" path, and adding one would be a wire change.

**Trailing bytes:** tolerated in HELLO2 only, and there deliberately — that
is the extension point. DIGEST, LAN_ENDPOINT and TRANSPORT_PROBE all require
the cursor to be exactly consumed; trailing garbage is malformed. HELLO has
no notion of trailing bytes at all, which is precisely why it is frozen
(`HELLO-01`).

**Versioned bodies:** LAN_ENDPOINT and TRANSPORT_PROBE each carry a version
byte and reject any value but `1`.

### 2.2 Message kinds

`MessageBody.kind`, inside the sealed payload:

| Kind | Meaning | Chat-visible | Persists a `msg_id` row |
|---|---|---|---|
| 1 | text | yes | yes |
| 2 | receipt (cumulative) | no | no |
| 3 | friend request | no | no |
| 4 | group invite | no | no |
| 5 | profile sync | no | no |
| 6 | friend directory | no | no |
| 7 | introduced friend request | no | no |
| 8 | LAN endpoint hint | no | no |
| 9 | relay-change notice | no | no |
| 16 | attachment manifest | yes | yes |
| 17 | attachment chunk (reserved, not yet produced) | — | — |
| 18 | reaction | no | yes |
| 19 | group metadata update | no | yes |

Two different questions are asked about a kind, and they have deliberately
different answers:

- **`core_kind_persists_msg_id_row`** — did consuming this envelope leave
  durable evidence naming its `msg_id`? Only kinds 1, 16, 18, 19. This is
  what `ACK-01` consults. Everything else records consumption separately.
- **`core_is_hidden_spray_kind`** — does this kind ride the outbound queue
  with no chat row, so no digest and no recent-id ack will ever retire it?
  Kinds 3, 5, 6, 7, 9. Receipts are hidden by the first question and
  deliberately not by the second.

**Unknown kind:** dropped at the unhandled-kind arm after opening. It is not
an error and not a disconnect; the envelope is simply not dispatched. This
is why capability bits exist — see 2.4.

### 2.3 Limits

| Limit | Value | Where |
|---|---|---|
| Max sealed envelope | 512 KiB | `MAX_ENVELOPE_SEALED_BYTES`, enforced independently by the relay |
| Envelope frame public header | 34 bytes | `ENVELOPE_FRAME_OVERHEAD` |
| Max peer frame | header + 512 KiB | `MAX_P2P_FRAME_BYTES` |
| Max BLE attribute value | 512 bytes | `MAX_ATT_VALUE_LEN` |
| BLE fragment header | 4 bytes (`index16 ‖ total16`, big-endian) | `framing.rs` |
| Max BLE fragments per frame | 65,535 | `MAX_FRAGMENTS` |
| Default hop TTL | 7 | `DEFAULT_HOP_TTL` |
| Default envelope expiry | 7 days | `DEFAULT_EXPIRY_MS` |
| Profile-sync avatar / name | 64 KiB / 128 bytes | `protocol.rs` |
| Friend directory | 64 KiB, 64 entries | `protocol.rs` |
| Relay-change notice URL / token / subject | 512 / 256 / 64 bytes | `protocol.rs` |

BLE reassembly is strictly ordered and single-frame-at-a-time. Any
out-of-order index, a total mismatch, an oversized fragment, or a cumulative
overrun discards the partial frame rather than reallocating around it.

### 2.4 Capability bits and additive compatibility

Capabilities are a `u32` little-endian bitfield in HELLO2. Both shells read
`core_own_capabilities()` rather than hardcoding bits.

| Bit | Name | Meaning |
|---|---|---|
| `1 << 0` | `CAP_ACKS_HIDDEN_KINDS` | Stores hidden kinds 3/5/6/7 as rows on receipt, so its delivered watermark advances past them |
| `1 << 1` | `CAP_RELAY_UPDATE` | Understands and stores kind 9 |

The additive-compatibility rules:

- A new kind that a hidden-spray sender must stop re-offering needs **its own
  bit**. It may not ride an existing bit, because an existing bit is a
  truthful claim about a *fixed* set of kinds — an older build advertising
  `CAP_ACKS_HIDDEN_KINDS` honestly still drops kind 9. Trusting the old bit
  for a new kind reintroduces exactly the mixed-version resend chatter HELLO2
  was added to end.
- Unknown bits are ignored. A peer advertising bits this build does not know
  is treated as not having whatever those bits mean.
- Trailing HELLO2 bytes are ignored, so future fields are additive.
- Legacy HELLO is frozen (`HELLO-01`).

## 3. Relay interface

Derived from `relayd/src/lib.rs`, `core/src/relay_wire.rs`, and
`core/src/relay_status.rs`.

### 3.1 Methods

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/healthz` | Liveness only. Not evidence of anything about a mailbox. |
| `POST` | `/envelopes` | Deposit one envelope into a family mailbox. |
| `GET` | `/envelopes` | Fetch a page of envelopes matching supplied recipient hints, ascending by row id from a cursor. |
| `POST` | `/envelopes/ack` | Delete named rows. Independent of the fetch cursor. |
| `POST` | `/presence` | Announce own hints and query others'. |
| `GET` | `/ws` | Push hint socket. |

Administrative routes exist under `/admin/families` and are out of scope for
client contract purposes.

### 3.2 Authentication classes

| Class | Recognisable by | May do |
|---|---|---|
| Member token | no class prefix | post, fetch, ack, presence, WebSocket |
| Deposit token | `cmdep1-` prefix | post only |

A deposit token is a one-way attenuation of its family's member token, so a
device can stamp one onto a friend card entirely offline and the relay
derives the identical value independently. Friend cards carry the deposit
class; the pass setup card carries the member class. Consequently a resolved
poll endpoint that would carry a deposit credential is dropped rather than
attempted — reading someone else's mailbox is prevented by the token class,
not by client politeness.

### 3.3 Stable response codes

The JSON `code` field is authoritative when present; a proxy can rewrite a
status, but the body comes from the relay itself. Unknown status/code
combinations degrade to a generic outage rather than being guessed at.

| Status | `code` | Client meaning | Self-heals |
|---|---|---|---|
| 403 | `family_expired` | Pass lapsed | no |
| 403 | `family_suspended` | Operator disabled the family | no |
| 401/403 | (none) | Saved credential is bad | no |
| 507 | `family_quota_exceeded` | Hosted storage full; posting fails while fetching still works | no |
| 413 | `envelope_too_large` | This one envelope can never be posted | no |
| 429 | `rate_limited` | Too fast, not broken; `Retry-After` present | yes |
| other non-2xx | — | Generic outage | yes |

When one pass observes several, a fixed rank decides which is shown:
suspended > expired > token rejected > mailbox full > message too large >
rate limited > outage.

`Retry-After` is integer delta-seconds, clamped by the client to 1–60, with a
30-second fallback when the header is missing or unparseable.

### 3.4 Sizes and caps

| Cap | Value |
|---|---|
| Client-requested fetch page | 256 rows |
| Server default / maximum fetch limit | 100 / 500 rows |
| Max response body the client will read | 12 MiB |
| Server fetch page sealed-byte budget | 8 MiB |
| Max fetch hints per request | 256 |
| Max ack ids per request | 512 |
| Max presence announce / query | 4 / 512 |
| Max WebSocket inbound message | 4 KiB |
| Max envelope sealed bytes | 512 KiB |
| Max server retention | 30 days |

The 256-row page size is a client choice, not a server contract. The server
may clamp it lower, and that must not end a walk — see `PAGE-01`.

### 3.5 Cursor and ack independence

The fetch cursor and acking are separate mechanisms and must stay separate.
Acking deletes rows; the cursor records how far a walk has read. A row can be
read and not acked (a carried copy kept as the durable fallback), and rows
below the cursor can be deleted by someone else without moving it. Nothing
about a successful ack may be inferred from a cursor position, and nothing
about a cursor position may be inferred from an ack.

Crucially, the cursor only ever covers the *hints that were sent*. Gaining a
contact or a group widens the hint set, so mail that arrived under a hint
this device did not yet have is already below the frontier where no sweep
interval reaches it. Core detects the hint-set change and drops the frontiers
so the next walk starts from zero.

### 3.6 Presence and push are hints

Presence answers and WebSocket pushes are hints about when it might be worth
running a pass. They are not delivery evidence, not arrival evidence, and not
proof a peer is reachable. A push wakes a pass; the pass decides. Losing
every hint degrades latency, never correctness.

## 4. Store-state terms

Derived from the schema in `core/src/store.rs`. These are the words the rest
of the contract uses; where a term names a table or column, that is the
authority.

**authored / outbound.** A message this device wrote, queued in
`outbound_envelopes` keyed by `msg_id` with a `dedupe_key`. It carries
`queued_at` and a nullable `relay_posted_at`. Today those two columns plus
`expiry` are the *entire* lifecycle, which is what `QUEUE-01` exists to fix.

**outgoing receipt envelope.** A cumulative receipt this device owes,
in `outgoing_receipt_envelopes`, keyed by `(chat_id, sender_user_id,
receipt_type)` — one row per stream, replaced upward, never accumulated.

**carried.** Someone else's envelope this device is muling, in
`carried_envelopes`, keyed by `msg_id`. `is_family` separates mail for known
contacts from foreign traffic; `from_relay` marks a copy fetched from a relay
so it is delivered over the mesh but never re-uploaded; `content_digest`
supports digest-proof removal; `relay_uploaded_to` is the `MARK-01` marker.

**consumed.** This device opened the envelope and durably stored the result.
For kinds that persist a `msg_id` row that evidence is the `messages` row;
for hidden kinds it is a `consumed_hidden_msg_ids` entry with the same
expiry as the envelope. Consumption is what `ACK-01` requires.

**seen.** This device recognises the `msg_id` but is not asserting it
consumed it. Ackable only under the narrow `ACK-01` conditions.

**relay-uploaded.** A carried row that reached a relay and holds a durable
marker naming the destination. Markers are first-writer-wins and are cleared
wholesale when a destination endpoint changes, because "already on the old
mailbox" says nothing about the new one.

**receipt watermark.** A cumulative lamport value per
`(chat_id, sender_user_id, receipt_type)`: "delivered/read through here".
Monotonic upward; a lower or replayed value never regresses it. Re-sending
the same or a higher watermark is always safe, which is what lets a lost
receipt heal itself.

**frontier.** The `relay_fetch_cursors` position for one mailbox key:
everything at or below is handled. The mailbox key is derived from the relay
URL and token but contains neither.

**sweep.** A periodic full re-walk from zero that catches rows below the
frontier — mail that arrived under a hint this device did not have, or a
mailbox whose id space was rebuilt. Sweeps have their own resume cursor so a
yielded sweep continues rather than restarting, and a completed sweep is what
licenses lowering a frontier.

## 5. Ordered relay pass stages

This is the deployed order, read from the Android engine and the iOS
controller as they stand. Package C0 pins it as an explicit stage enum; until
then it is documented here so a change is visible as a change.

The two shells do not agree everywhere. Where they differ, this section states
both and section 5.2 records the divergence as a row, because a contract that
silently writes down one shell's behaviour hides exactly the change it exists
to make visible.

1. **Prune and repair local state.** Expire outbound envelopes, outgoing
   receipt envelopes, carried rows, and consumed-hidden records. Restore
   persisted contact-endpoint health and begin this pass's provisional
   observations. Backfill missing outgoing receipts.
2. **Announce only when changed.** If this device's own relay endpoint
   changed since the last announcement, queue the notice to every contact so
   it rides out in this same pass. Idempotent, so a periodic poll re-entering
   here costs nothing.
3. **Upload receipts.**
4. **Upload locally authored rows.**
5. **Upload carried rows,** writing the durable upload marker on success
   (`MARK-01`).
6. **Decide hint-triggered rewalk.** If the hint source set changed, drop
   frontiers so the walks below start at zero.
7. **For each eligible config: presence, and the mailbox page
   walk/process/ack.** The two run in the opposite order on the two shells
   today — iOS presence first, Android walk first — and they treat a presence
   failure differently as well. Neither ordering is written down anywhere as
   deliberate. See 5.2; C0 has to choose one, migrate the other shell onto
   it, and say so in the same PR.
8. **Commit silence and rejection evidence and fold pass health.** Silence
   may only be committed now, because only now is it known whether this
   device's own mailbox answered (`SILENCE-01`).
9. **Finish, or schedule a continuation with an explicit progress reason**
   (`PROGRESS-01`).

### 5.1 Abort and yield points

| Point | Trigger | Effect |
|---|---|---|
| **Family rate-limit abort** | first `429` on family work, any stage | Ends all remaining network stages of the pass. Records the quiet window. Not an error state for the person holding the phone. |
| **Per-config fault** | a non-abort failure against one relay config | That config is skipped; remaining configs still run. A contact's stale credential must not stop this device polling its own mailbox. |
| **Walk budget yield** | pages or envelopes for this pass exhausted | The walk stops mid-mailbox, records its resume position, and requests a continuation. |
| **No configs** | no own pass and no contact endpoint | Pass ends immediately in a no-config state. |
| **Contact endpoint breaker** | a contact endpoint silent this pass | Provisional within the pass; committed only at stage 8, and only with proof another relay answered. |
| **Post-upload cap** | a family-scale work cap on carried upload | Bounds one pass's uploads so a deep carry queue cannot starve receipts or authored rows. |

Ordering constraints that are load-bearing rather than incidental: receipts
before authored before carried (a receipt is small and unblocks a peer's
queue); announce before every upload (so a changed endpoint rides the same
pass); rewalk decision before the walks; silence commit after the walks.

### 5.2 Known cross-shell divergences inside a stage

Places where reading the two shells gave two answers. They are recorded rather
than resolved: this revision changes no behaviour, and picking a winner is a
migration with a canary, not a documentation edit. Each row names the package
that must choose. The list is what reading the stages for this revision turned
up; it is append-only, and finding another one is a finding, not a failure.

| Divergence | Android today | iOS today | Whose choice |
|---|---|---|---|
| Order of presence and the mailbox walk within stage 7 | walk first, then presence, per config | presence first, then the walk, per config | C0 pins one in the stage enum and migrates the other shell in the same PR |
| What a presence failure costs | swallowed and logged; only a family rate limit escapes, so presence never marks the config faulted | recorded against the config like any other fault, and the walk still runs afterwards | C0, with the same stage enum |

Neither is known to be load-bearing, which is the point: an undocumented
difference cannot be reasoned about, and a migration that quietly changes
Android's deployed ordering to match a written stage enum would otherwise
land with no invariant, no fixture, and no row saying it was ever different.

## 6. Fixture and event schema — `cruisemesh.protocol-event/v1`

Fixtures live in `core/tests/fixtures/` as JSONL: one JSON object per line,
UTF-8, LF-terminated. The same schema serves three consumers — core replay
fixtures, simulation and decision-shadow transcripts, and the redacted
archive a person exports from Advanced diagnostics — so an exported archive
is accepted by the replay command with no conversion step.

At this revision the validator enforces schema, redaction, ordering, and
declared invariant ids. Behaviour execution arrives with the replay runner in
package C0.

### 6.1 Header record

Exactly one, and it is the first line.

```json
{"schema":"cruisemesh.protocol-event/v1","record":"header","fixture":"short-page",
 "title":"A clamped page is not the end of the mailbox","origin":"synthetic",
 "public_reference":"#270","pseudonyms":["peer-a"],
 "expect_invariants":["PAGE-01","CURSOR-01"]}
```

| Field | Required | Rule |
|---|---|---|
| `schema` | yes | exactly `cruisemesh.protocol-event/v1` |
| `record` | yes | `header` |
| `fixture` | yes | matches the filename stem |
| `title` | yes | one line, for a stranger |
| `origin` | yes | `synthetic` or `redacted-field-archive` |
| `public_reference` | no | a public PR or issue reference, e.g. `#283` |
| `pseudonyms` | yes | every archive-local actor name events may use |
| `expect_invariants` | yes | non-empty; every id must exist in section 1 |

### 6.2 Event record

```json
{"record":"event","seq":4,"at_ms":1700000004000,"code":"frontier_held",
 "pass":"p1","action":3,"actor":"peer-a","invariants":["CURSOR-01"],
 "counts":{"rows_processed":12,"rows_acked":0},"outcome":"ack_failed"}
```

| Field | Required | Rule |
|---|---|---|
| `record` | yes | `event` |
| `seq` | yes | starts at 1, strictly `+1` per record |
| `at_ms` | yes | explicit time, non-decreasing |
| `code` | yes | from the registry in 6.3 |
| `session`, `pass` | no | opaque short ids |
| `action` | no | non-negative integer |
| `actor` | no | must be a declared pseudonym |
| `invariants` | no | ids from section 1; each must also appear in the header's `expect_invariants` |
| `counts` | no | flat object, non-negative integers only |
| `outcome` | no | short stable token, never free prose |

### 6.3 Stable event codes

Codes are API. Prose log messages are not.

`pass_start`, `pass_finish`, `action_emitted`, `action_result_accepted`,
`action_result_stale_ignored`, `rate_limit_abort`, `frontier_held`,
`frontier_advanced`, `continuation_scheduled`, `endpoint_rested`,
`endpoint_recovered`, `carried_row_marked`, `budget_yield`,
`shadow_mismatch`, `invariant_violation`, `page_ingested`,
`receipt_watermark_observed`, `outbound_queue_scanned`, `spray_planned`,
`silence_observed`, `request_rejected`.

### 6.4 Redaction rules

Every identity in a checked-in fixture is synthetic. Field-derived fixtures
carry archive-local pseudonyms, never a real user id — not even a hashed one,
because a hash of a stable id is still a stable id.

The validator rejects a fixture that contains any of: a token prefix
(`cmdep1-`), a friend-card prefix (`CMFRIEND`), a deep-link scheme
(`cruisemesh://`), any `://` URL, an `Authorization` or `Bearer` header
fragment, a PEM key header, a private-address literal, or a JSON key outside
the schema. Payloads are represented by kind, length and digest — never
bytes.

### 6.5 Named incident fixtures

| Fixture | Incident | Declares |
|---|---|---|
| `sweep-livelock.jsonl` | a sweep that rescheduled forever without advancing (#270) | `PROGRESS-01`, `LIVE-01`, `CURSOR-01` |
| `carry-storm.jsonl` | carried rows re-uploaded every launch for want of a marker (#222) | `MARK-01`, `CARRY-01`, `LIVE-01` |
| `watermark-lock.jsonl` | receipt repair self-gated at a zero watermark (#241) | `WM-01`, `PROGRESS-01` |
| `watchdog-spray.jsonl` | carried-first spray large enough to trip a watchdog (#280) | `SPRAY-01`, `LIVE-01` |
| `429-mid-receipts.jsonl` | a family rate limit arriving mid-upload (#260, #261) | `RATE-01`, `LIVE-01` |
| `short-page.jsonl` | a server-clamped page mistaken for EOF | `PAGE-01`, `CURSOR-01` |
| `ack-fail-after-consume.jsonl` | durable consume, then a failed ack, then a restart | `TXN-01`, `CURSOR-01`, `IDEMP-01` |
| `oversize-shrink.jsonl` | a page over the response cap, retried smaller | `PAGE-01`, `LIVE-01` |
| `contact-silence-no-proof.jsonl` | a silent contact endpoint with no proof of own connectivity | `SILENCE-01` |
| `pending-rerun-during-backoff.jsonl` | a pending nudge trying to start a pass inside the quiet window | `RATE-01`, `PROGRESS-01` |
| `zombie-outbound-queue.jsonl` | an outbound queue that never retires anything (#283) | `QUEUE-01`, `LIVE-01` |

## Appendix A — ownership inventory

Every protocol-relevant place where a decision is made twice, verified
against the tree as it stands. Labels:

- **hoist-now** — a named work package is already going to move it.
- **hoist-later** — it should move, but no package owns it yet.
- **shell-forever** — it is an OS driver or lifecycle concern and belongs in
  the shell.
- **presentation-only** — the parity is about wording or layout, not
  protocol.
- **delete** — a duplicate that can simply go once its owner lands.

| Concern | Android today | iOS today | Shared today | Label | Destination |
|---|---|---|---|---|---|
| Family request pacing and 429 backoff | `mesh/FamilyRelayBackpressure.kt` | `Relay/FamilyRelayBackpressure.swift` | fault classification and the `Retry-After` clamp in `relay_status.rs` | hoist-now | `relay_policy.rs`; B0/B2 |
| Pending relay rerun | `mesh/RelayRerunPolicy.kt` + `RelaySyncEngine.kt` | rerun path in `MeshController.swift` | none | hoist-now | `relay_policy.rs`, then `relay_pass.rs`; B0/C0 |
| Mailbox per-pass work/yield budget | none — `RelayMailboxWalkBudget.kt` was removed outright in #270; only the call-through `RelayMailboxWalkBudgetTest.kt` remains | none — `Relay/RelayMailboxWalk.swift` calls `relayMailboxWalkAction` directly and never had a local copy | `relay_cursor.rs` owns pages, envelopes, continuation delay (#270) | delete | already done in #270; nothing left to remove, and the Kotlin test stays as the guard that Android still reaches core |
| Mailbox walk execution | `mesh/RelayMailboxWalker.kt` (#276) | `Relay/RelayMailboxWalk.swift` (#276) | walk action, sweep due, resume, frontier in `relay_cursor.rs` | hoist-now | `relay_pass.rs`; C4 |
| Relay pass stage order | `mesh/RelaySyncEngine.kt` | `relaySyncBlocking` region of `MeshController.swift` | none — and the two shells already disagree inside stage 7, see 5.2 | hoist-now | `relay_pass.rs`; C0–C5, which must resolve the 5.2 rows explicitly rather than by picking whichever shell it reads first |
| Relay HTTP execution and page-size cap | `relay/RelayClient.kt` | `Relay/RelayClient.swift` | codecs, caps and status classification in `relay_wire.rs` / `relay_status.rs` | hoist-now (semantics) / shell-forever (transport) | core request/response semantics, native execution; C0–C2 |
| Sweep, frontier, ack, continuation | `RelaySyncEngine.kt` + `RelayMailboxWalker.kt` | `MeshController.swift` + `Relay/RelaySweepSession.swift` | `relay_cursor.rs` + `store.rs` helpers; frontier lowering is core (#279) | hoist-now | `relay_pass.rs` + a transactional store API; C4 |
| Contact rejection / silence / rest | `mesh/ContactRelaySilence.kt` + engine | `ContactRelaySilence` in `RelaySweepSession.swift` + controller | `contact_relay_health.rs` + persisted store state | hoist-now | policy in B0, orchestration in C3/C4 |
| Relay pass health fold | `mesh/RelayFaultPolicy.kt` + `MeshConnectivityStatus.kt` | `MeshConnectivityStatus.swift` + controller | rank and classification in `relay_status.rs` | hoist-now | core snapshot and reason codes; B0/D3 |
| Connection and delivery health classification | consumes core (#281, #282) | consumes core (#281, #282) | `connection_health.rs` owns classification, per-recipient delivery, receipt-gated lines | presentation-only | stays core; shells render |
| Failover resume debounce | `mesh/FailoverResumeDebounce.kt` — thin wrapper | equivalent wrapper | `transport_policy.rs` owns the window and coalescing (#269) | presentation-only | stays core; wrappers are adapters |
| Peripheral link admission and spray cooldown | `mesh/PeripheralLinkAdmission.kt`, spray cooldown classes (#277) | not yet extracted | none | hoist-later | `mesh_meet.rs` when D2 lands; the byte/cadence half is `SPRAY-01` |
| Per-encounter spray byte budgets | three constants in `mesh/InboundEnvelopeProcessor.kt` (carried 256 KiB, own outbound 256 KiB, receipts 64 KiB) | the same three in `Core/ProtocolKinds.swift` | `core_digest_spray_plan` spends the budgets and resumes from a carried cursor, but every value is passed in by the shell | hoist-now | the numbers belong beside the plan in `mesh_meet.rs`; D2. Equal today, and nothing makes them stay equal |
| Inbound envelope disposition | `mesh/InboundEnvelopeProcessor.kt` | `processInboundEnvelope` and handlers in `MeshController.swift` | crypto/store primitives and ack eligibility in `engine.rs` / `store.rs` | hoist-now | `mesh_receive.rs`; D0/D1 |
| HELLO / digest / carry encounter | `MeshService.kt` + `InboundEnvelopeProcessor.kt` | `MeshController.swift` | digest and spray planning in `engine.rs`, session state in `transport_policy.rs` | hoist-now | `mesh_meet.rs`; D2/D3 |
| Logical peer routing | `MeshRouter.kt` / `MeshRouterState.kt` | `MeshRouter.swift` / `MeshRouterState.swift` | `CoreMeshRouterState` in `transport_policy.rs`; peer collapse is core (#266) | hoist-later | extend the existing core router; D2 |
| LAN endpoint cache and provenance | `mesh/LanEndpointCache.kt` | `LanEndpointStore.swift` | `lan_util.rs` owns provenance, eviction and same-network checks (#271, #278) | presentation-only | stays core |
| LAN endpoint hint authoring | `mesh/LanEndpointSender.kt` + `LanEndpointSendPolicy.kt` | full twin: `Mesh/LanEndpointSender.swift`, plus `sendLanEndpointHint` / `queueCurrentLanEndpoint` in `MeshController.swift` | encoder and host validation in `protocol.rs` | hoist-later | `ENDPOINT-01`'s authoring half; D2/D3 must move **both** copies |
| LAN scan and socket lifecycle | `LanTransport.kt` and scan files | `LanTransport.swift` and scan files | primitives in `lan_util.rs` / `lan_session.rs` | shell-forever (drivers) | shared progress policy in D2/D3 |
| BLE central / peripheral lifecycle | `BleCentral.kt`, `BlePeripheral.kt` | `BleTransport.swift` | framing only, in `framing.rs` | shell-forever | — |
| Push, OS polling, background wake | `relay/RelayPushClient.kt` + service scheduling | `Relay/RelayPushClient.swift` + controller scheduling | none | shell-forever | push stays a pass nudge only |
| Outbound queue retirement | none | none | `outbound_retirement.rs` owns coverage retirement, supersession and per-kind expiry; `store.rs` executes them at receipt time and on open (#283) | presentation-only | stays core; no shell decides any of it |
| Delivery / transport / health UI | Compose status surfaces | SwiftUI status surfaces | semantic facts in `connection_health.rs` / `semantic.rs` | presentation-only | core facts, native presentation; D3 |
| Field diagnostics archive | `debug/DiagnosticsShare.kt` | `UI/DiagnosticsArchive.swift` | delivery metrics only | hoist-now | shared event JSONL + native wrappers; B1 |
| Multi-node orchestration test | n/a | n/a | `core/tests/mesh_sim.rs` reimplements receive and meet | delete | production `mesh_receive` / `mesh_meet`; D0/D2 |
| Generated bindings | ignored `kotlin-gen/` | checked-in `ios/CruiseMesh/Generated/` | `core/src/lib.rs` exports | shell-forever (mechanism) | drift is blocking in the Rust workflow (#269); keep it blocking |

### A.1 Rows that changed since the inventory was first drafted

Recorded so a reader does not have to diff two documents:

- The mailbox walk budget and the sweep resume policy are **core** now
  (#270), and the Android class did not become a shim — `RelayMailboxWalkBudget.kt`
  was deleted outright in that PR. What survives is
  `RelayMailboxWalkBudgetTest.kt`, which calls the core functions directly and
  is worth keeping for exactly that reason. iOS never had a budget class at
  all; `RelayMailboxWalk.swift` calls `relayMailboxWalkAction` itself. So the
  row is `delete` and the deletion has already happened — there is nothing
  left for B2 to remove here.
- The walk itself is **extracted on both shells** (#276) with a scripted
  fake-relay wiring harness on each, which is what makes `PAGE-01` and
  `CURSOR-01` testable outside a device.
- Frontier lowering after a completed sweep is **core policy** (#279), which
  is why `CURSOR-01` is stated as "never backward on a normal pass" with the
  sweep repair named explicitly.
- Failover-resume debounce is **core** (#269); both shells wrap it.
- `connection_health.rs` exists (#281, #282) and owns health classification,
  per-recipient delivery status, and receipt-gated delivery lines — so
  `UI-01` has a real core owner rather than only native UI tests.
- Peripheral link admission and the notify-reject spray brake are **plain
  shell classes** (#277) with no core owner yet.
- LAN endpoint hint authoring is **not** Android-only. iOS carries a full
  twin — `Mesh/LanEndpointSender.swift` for the kind-8 hint envelope, and
  `sendLanEndpointHint` in `MeshController.swift` for the `0x04` frame. D2/D3
  owns two copies, not one, and a hoist that moves only the Kotlin file
  leaves `ENDPOINT-01`'s authoring half exactly as split as it is today.
- The per-encounter spray byte budgets are still duplicated constants on both
  shells. Core spends them; neither shell reads its numbers from core.
- Stage 7 is not one order. Android walks the mailbox and then syncs
  presence; iOS syncs presence and then walks. Section 5.2 carries this and
  the presence-failure difference beside it.
- Outbound queue retirement is **core** (#283) and never was split: neither
  shell held a copy of it, so `outbound_retirement.rs` had nothing to hoist,
  only a gap to fill. Both shells got the smaller queue with no code change,
  because both read it through store calls that already existed.
