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
| `RATE-01` | The first family 429 ends remaining pass network work. `Retry-After` is a floor, and pending nudges cannot bypass the quiet window. | core | `core/src/session/relay_policy.rs` owns request pacing, the exponential curve and its cap, the `Retry-After` floor, the stable identity jitter, the pending-rerun decision and the pass health fold; `relay_status.rs` owns the header clamp and the classification. Both shells delegate. The first clause — aborting the remaining stages — is completed by C0's pass |
| `ENDPOINT-01` | A phone advertises only its own endpoint. A discovered or third-party address is never forwarded to anyone. | hoist-pending | receive-side scoping is core; hint authoring is shell-owned |
| `SILENCE-01` | Contact silence advances only with same-pass proof that another relay answered; authoritative rejection does not require that proof. | core | `core/src/contact_relay_health.rs` silence tests; index re-asserts the delta rule |
| `UI-01` | Delivery and via-transport claims require persisted arrival or receipt evidence, never a current-link guess. | core | `core/src/connection_health.rs` delivery tests; index re-asserts the queue-honesty gate |
| `LIVE-01` | Every pass terminates inside its declared request, envelope, byte, and time/yield budgets. | core | `core/src/session/relay_pass.rs` declares the budgets and carries them in every summary; `core/tests/relay_pass_replay.rs` drives a real pass against four hostile relays (endless mail, a cursor that never moves, silence, blanket rejection) and every incident fixture |
| `PROGRESS-01` | A continuation must strictly advance a frontier/work cursor or strictly increase a future deadline/backoff. Unchanged-state reschedule loops are forbidden. | core | `core/src/relay_cursor.rs` walk-budget yield plus `core/src/session/relay_pass.rs`, which can only emit a continuation carrying a `CoreRelayProgressReason`; `core/tests/relay_pass_replay.rs` gathers the continuations several passes produce and checks the rule across all of them |
| `MARK-01` | A successfully relay-uploaded carried row is durably marked before the pass ends; the marker survives restart and suppresses repeat upload for its lifetime. | core | `core/src/store.rs` upload-marker tests; index re-asserts first-writer-wins |
| `WM-01` | Receipt repair has a reachable, bounded path from every supported stored state; a peer watermark of zero cannot permanently gate repair. | hoist-pending | shell repair planners |
| `SPRAY-01` | Carried-first work toward one peer is bounded per encounter **in bytes as well as in rows**, and re-offers to the same peer are rate-gated, so a large carrier cannot starve receive work or trip an OS watchdog. | core | `core/src/spray_policy.rs` owns cadence, identical-set suppression, the three per-encounter byte budgets, a per-link burst allowance, and the receipt-quiet backoff (#280); both shells consult it and hold no spray constant of their own |
| `HELLO-01` | Legacy HELLO never gains trailing fields; new capabilities use HELLO2 frame `0x06`. | core | `core/src/protocol.rs` HELLO/HELLO2 codec tests; index re-asserts both shapes |
| `IDEMP-01` | Duplicate, late, or replayed external results cannot double-apply a mutation, regress a cursor, or consume a carried row. | core | `CoreRelayPass::resume_http` compares `pass_id`/`action_id` against the single outstanding action; `core/tests/relay_pass_replay.rs` permutes duplicate, future-id, stale-id, wrong-pass, late-after-finish and cancellation against a clean run and requires an identical store |
| `TXN-01` | No store transaction spans external I/O. Page consume and frontier advancement retain their documented two-transaction crash safety. | core | `MessageStore::ingest_relay_page` is one transaction that commits before it returns, and the action/result seam makes the boundary structural; `core/tests/relay_pass_replay.rs` kills a pass between the consume and its ack and relaunches |
| `QUEUE-01` | Proof of delivery for a 1:1 outbound envelope permits — and the queue eventually performs — its retirement, and a payload whose usefulness is shorter than its expiry is superseded rather than re-advertised. The advertised outbound set shrinks under coverage; flat expiry is a backstop, never the only retirement path. | core | `core/src/outbound_retirement.rs` coverage, sweep, supersession and expiry tests (#283); index re-asserts that a delivered watermark shrinks both readers of the queue |
| `SECRET-01` | Events, fixtures, summaries, and exported diagnostics contain no relay tokens, raw friend cards, plaintext, private keys, or full endpoint-bearing bodies. | core | three layers: `core/src/protocol_event.rs` refuses to store a record that trips a canary or carries an undeclared key, `core/tests/protocol_event_ring.rs` runs the canary against a live store's export, and `core/tests/protocol_contract.rs` scans the checked-in fixture corpus |

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

Each phone adds a small stable offset to the window so a family's phones do
not wake in lockstep and re-collide. The offset is derived in core from the
*public* user id under a BLAKE2b domain-separation context, and that is a
change of substance rather than of address: both shells were previously
seeding it from a platform hash of their own — Android from
`ByteArray.contentHashCode()`, iOS from a hand-written FNV-1a added because
Swift's `hashValue` is process-randomized and would not have been stable at
all. Two shells hashing the same identity two ways is two answers to one
protocol rule, and neither answer was written down anywhere. The window's
shape is unchanged; which phone draws which offset inside it moves once.

The offset takes a public value on purpose. It is observable in request
timing, so anything secret fed into it would leak at whatever rate the phone
gets rate limited.

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

The budgets are values, not comments: `CoreRelayPassBudgets` rides into a pass
in its plan and back out in its summary, so a transcript proves the bound
rather than a reader inferring it. The wall-clock half is measured from the
times the driver reports, which is the correct division of labour — core
cannot time out a socket it cannot see, so a pass whose driver has stopped
answering is bounded by the driver's own timeout and by nothing here.

Two of the numbers are *exact* and two are *admission* limits, and a summary
is read against them differently. `max_requests` is exact: the ack a consumed
page earns is counted against it like any other request, and a page that
cannot afford one holds its frontier and comes back next pass rather than
spending a request the budget does not have. `max_envelopes` and
`max_response_bytes` are checked before a request is admitted, so the last
page admitted can carry a pass past them — by at most that page, or that
body. Bounding them exactly would require predicting a page's size before
asking for it.

The evidence has to be scenarios where the budget is the thing that stops the
pass. At deployed settings the per-mailbox walk budget cuts in first, so a
default-settings test proves nothing about the pass-level gate; the owning
suite therefore holds passes to budgets small enough to bite — three
requests against endless mail, ten envelopes at eight rows a page, a deadline
shorter than one answer — and each of those scenarios fails if the gate is
removed.

#### `PROGRESS-01` — a continuation must buy something

Rescheduling is permitted only when the reschedule strictly advances a
frontier or work cursor, or strictly increases a future deadline or backoff.
A continuation that leaves state unchanged is a livelock with good manners,
and it is exactly what emptied batteries before #270.

`CoreRelayPass` is deliberately stricter than the rule. A continuation exists
only when the pass strictly advanced a cursor, durably ingested rows, or wrote
an upload marker — or when it is deferring into a quiet window strictly later
than the one in force when it began. A pass that did neither ends with no
continuation at all, so an unchanged-state reschedule is unrepresentable
rather than merely forbidden.

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

Four decisions carry that, all in `core/src/spray_policy.rs`:

- **Cadence.** A verified first encounter is never gated — two phones meeting
  and beginning to sync is the product, and delaying it would break the thing
  the mesh is for. Everything else is: a reconnect, a peer digest outside the
  exchange window our own spray opened, and the maintenance tick each wait out
  an interval. Core classifies first contact from its own record, so a shell
  that labels every reconnect as a fresh encounter is silently downgraded
  rather than believed.
- **Identical-set suppression, per lane.** Core digests the `msg_id` set each
  of the three lanes advertises — foreign carry, own outbound, own receipts —
  and decides each on its own. The union would be the wrong unit: the recorded
  shape was an authored set invariant at 16 envelopes across 28 consecutive
  sprays beside a carried lane walking a cursor, and any carried page turn
  changes a union digest, so a union would suppress nothing. An unchanged lane
  is not re-offered until a bounded re-offer interval lapses — aligned with the
  carried re-walk interval, because both exist so that a frame lost in a link's
  FIFO is eventually found again. Any change sprays that lane immediately, and
  a lane that selected nothing is neither suppressed nor remembered as offered.
- **Byte budgets.** The three per-encounter budgets are core constants, and a
  per-link allowance bounds what may be *queued* at one link across every lane
  and every trigger. It is charged for everything the shells queue, not only
  the plan: the receipt repair pass, the per-missing-message re-send loop, the
  group catch-up that restarts at lamport 0 and the carry drain are all larger
  than the plan and none of them appear in it. The allowance is not reset by a
  disconnect either, because a disconnect is exactly what reconnect churn
  produces. A link that has been quiet is not throttled; a second encounter's
  worth of bytes in the same breath waits for the radio.
- **Receipt-quiet backoff.** A peer whose sprays keep producing no evidence of
  progress waits longer, up to a ceiling. This **caps waste; it never concludes
  brokenness** — a courier holding mail for someone who is not present produces
  no receipts and is behaving correctly. It stretches only sprays *we* start:
  the peer's own digest keeps the base interval, because that is the one path
  that sends the receipts we owe it and the backlog its watermark asked for,
  and quietness on the foreign-carry lane says nothing about either.

Every one of those is a *delay with a computable expiry*, never a drop. A
suppressed offer advances no cursor, records no hidden-kind offer, removes
nothing and acks nothing: `CARRY-01` and `ACK-01` are untouched by all of it.
Three gates now stand between a peer and a spray — the post-reject cooldown
(#277), the failover debounce (#269), and this cadence gate — and the core
suite carries a simulation proving their composition cannot starve a
legitimate peer.

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

One action is outstanding at a time, which is what makes this one comparison
rather than a per-lane problem: `CoreRelayPass::resume_http` checks the
result's `pass_id` and `action_id` against the single action it is waiting
for, and any mismatch restates that action unchanged, counts the result in
`stale_results_ignored`, and emits `action_result_stale_ignored`. A count that
keeps moving after the pass has finished is deliberate: a driver whose socket
completed twenty minutes late deserves to appear in the summary.

The comparison is only as good as the id. Action ids restart at 1 in every
pass, so the pass id is what separates a late answer from the pass before
from the answer this pass is waiting for — which means two live passes must
never share one. `CoreRelayPass::new` therefore *derives* the id it carries
from the label it was given, with a suffix unique in the process, rather than
using the label directly. The test that pins it keeps the outstanding action
id and changes only the pass id, because a permutation that changes both is
decided by the action id alone and proves nothing about this half.

A result that arrives before `start` is inert in the same way and is *not*
allowed to start the pass: `start` is what records the time the pass's
deadlines are measured from and where the quiet window is honoured, so a
replayed result that ran the stage machine would run a pass from time zero,
straight through a window it was built inside. This is the restart-recovery
shape — a driver that persisted an in-flight result and replays it against a
freshly built pass after process death.

#### `TXN-01` — never hold a transaction across the network

No store transaction spans HTTP, Bluetooth, LAN, a timer, or a platform
callback. Page ingestion and frontier advancement are deliberately two
separate short transactions with the network ack between them, so a crash at
any point replays safely: a consumed-but-unacked page re-presents and the
frontier stays put.

The action/result seam makes this structural rather than remembered. Every
store call a pass makes happens between an action being emitted and the next
being formed; the function that opened a transaction has returned before the
driver is handed anything, so there is no shape in which one can be held
across the wait. `MessageStore::ingest_relay_page` is the first transaction
and `MessageStore::advance_relay_fetch_cursor` the second, and a failed ack
runs the second one anyway — with `page_fully_processed` false, so it records
`frontier_held` and moves nothing. Not-doing-it leaves no evidence; holding it
explicitly does. Every ack failure runs it, including a `429`: a rate-limited
pass is the transcript that most needs to explain itself, and it must not be
the one where a consumed page's frontier goes unrecorded.

`ingest_relay_page` takes the pass and action ids that asked for the page and
puts them on the `page_ingested` record, so the one record that says the
first transaction happened joins the `action_emitted` above it and the ack
below it. A transcript that could not be read one pass at a time at exactly
that point would be missing it where it matters most.

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

Three boundaries are part of the rule rather than details of one
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

**A re-seal answers the peer; it does not re-admit the row.** The two
watermarks in the previous paragraph do not move together: a digest reports
the *contiguous* watermark and retirement follows the *MAX*, so any hole in
a peer's copy of our stream makes it ask to rebuild rows we have already
retired — routinely, on a field device, not as an edge case. The rebuilt
envelope must therefore go on the link that asked and nowhere else. If it
rejoined the outbound queue, the advertised set would regrow within one link
session, mail the recipient already acknowledged would be re-posted to the
relay, and the rule would hold for minutes at a time and never longer. A
rebuild also keeps the message's own persisted `msg_id`: a retransmission
that arrives under a fresh identity is new traffic to every dedupe set on
both sides, which is the resend chatter `HELLO-01`'s capability flags exist
to bound.

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

The rule covers the live event ring as well as the checked-in corpus, and it
is enforced in three places rather than one, because a redaction that depends
on every call site being careful is not a rule:

- **By construction.** A `ProtocolEventDraft` has no field a payload can
  arrive in. Its outcome, its count keys and its invariant ids are
  `&'static str` chosen at the call site; its actor can only come from the
  pseudonym allocator. The single exception is the generic violation hook,
  whose outcome is gated on being a short lowercase token.
- **Before storage.** `core/src/protocol_event.rs` scans each serialised
  record for the canary list below, and a record that trips one is replaced by
  an `invariant_violation` naming `SECRET-01`. The attempt survives; its
  contents do not.
- **On the corpus and on live exports.**
  `core/tests/protocol_contract.rs` scans every fixture, and
  `core/tests/protocol_event_ring.rs` plants a token in a relay config key, a
  contact id and a message payload, drives the real emit points, and proves
  none of it reaches the archive — with a negative control that tampers with a
  clean archive and requires the scanner to fail.

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

This is the order, and since package C0 it is an executable one:
`CoreRelayStage` in `core/src/session/relay_pass.rs` is the enum, and
`CoreRelayPass` walks it. The shells still run their own engines — C0 is dark,
and C1–C5 migrate them — so until then this section describes two
implementations that agree with the enum and, in three places, did not agree
with each other.

Where the shells differ, section 5.2 records the divergence as a row, because
a contract that silently writes down one shell's behaviour hides exactly the
change it exists to make visible. C0 owned three of those rows and decided
them; the decisions and their reasons are in the table there.

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
7. **For each eligible config: presence, then the mailbox page
   walk/process/ack.** Presence first is now pinned (C0). The walk is the
   budgeted, abortable, unbounded-input stage — it yields on
   `relay_mailbox_walk_action`, a `429` cuts it short, and on a phone with a
   deep mailbox it reaches its budget every pass — so a device that runs the
   walk first never announces presence at all. Presence is one fixed-cost
   request that has already happened before anything can consume the budget.
   A presence failure is recorded against the config and does not skip the
   walk. See 5.2.
8. **Commit silence and rejection evidence and fold pass health.** Silence
   may only be committed now, because only now is it known whether this
   device's own mailbox answered (`SILENCE-01`).
9. **Finish, or schedule a continuation with an explicit progress reason**
   (`PROGRESS-01`).

### 5.1 Abort and yield points

| Point | Trigger | Effect |
|---|---|---|
| **Family rate-limit abort** | first `429` on family work, any stage | Ends all remaining network stages of the pass. Records the quiet window *at the refusal*, as a floor (5.2). Stage 8 still runs, because evidence gathered before the refusal is real. Not an error state for the person holding the phone. |
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

Places where reading the two shells gave two answers. A row stays here until a
package resolves it, and a resolved row keeps its history rather than being
deleted: the value of this table is that it says a difference *existed*. Each
open row names the package that must choose. The list is append-only, and
finding another one is a finding, not a failure.

| Divergence | Android today | iOS today | Whose choice |
|---|---|---|---|
| Order of presence and the mailbox walk within stage 7 | walk first, then presence, per config | presence first, then the walk, per config | **C0 — resolved toward iOS, which is also the order the plan pinned.** The walk is the budgeted, abortable stage; presence sits behind it under Android's order and is therefore never reached on a device whose walk exhausts its budget every pass, which is exactly the device whose presence matters. Presence is one fixed-cost request. Cost of the change: one extra round trip of latency before the first fetch on a shallow mailbox, paid only by devices for which the walk was never the constraint. C1/C2 migrate Android |
| What a presence failure costs | swallowed and logged; only a family rate limit escapes, so presence never marks the config faulted | recorded against the config like any other fault, and the walk still runs afterwards | **C0 — resolved toward iOS.** `SILENCE-01` needs same-pass evidence, and a swallowed failure destroys it: a config whose presence failed and whose walk then succeeded is *not* silent, and one where both failed is stronger evidence than the walk alone, but swallowing made the two indistinguishable. Recording never skips the walk on either reading. C1/C2 migrate Android |
| When the quiet window is committed after a 429 | committed inside the failing request, as `max(existing, now + delay)`, so an earlier longer window survives a later shorter one | accumulated as `max` across the pass and committed once at the end, overwriting whatever was there | **C0 — resolved toward Android.** The window is a floor a later, shorter one cannot lower, which is what `RATE-01` says it is, and it exists from the refusal onward rather than from the end. `CoreRelayPassSummary::quiet_until_ms` is set the moment the refusal is seen, so a pass *cancelled* afterwards — an app backgrounded mid-pass — still reports it, where an accumulate-at-the-end pass reports nothing. Scope, stated plainly: this does **not** survive process death. Nothing in core persists the window; it lives in the pass object and in the summary, and Android's `rateLimitedUntilMs` is an in-memory field too, so neither shell has that property today. Making the floor durable is adapter work — a shell persisting the summary — and belongs to C1/C2 with the migration |
| Where presence is announced and queried | per poll config: a contact on another family's relay has their hint queried on that relay | own mailbox only | **C0 — resolved toward iOS, and it costs something.** Announcing this device's hints into another family's mailbox tells that family we exist, which is a privacy cost with no visible benefit; the query half carries no such cost but is dropped with it, so a contact reachable only through another family's relay stops resolving a last-seen time once the shells migrate. Recorded here rather than left in a code comment because it was found while writing `relay_pass.rs` and was not in this table before. C3 decides whether to reinstate the query alone before it migrates the shells |
| What a group-addressed authored row costs | one row per member: `RelaySyncEngine.kt` reads the group, builds the per-member fan-out rows with `coreGroupFanoutRows`, posts all of them, and marks the envelope relay-posted only when every row landed | the same fan-out in `MeshController.swift` | **C3 — open, and it blocks defaulting the core engine.** `session/relay_pass.rs`'s upload lanes post one row to one resolved mailbox and do not decompose a group-addressed row at all, so a group envelope would go out as a single group-hinted row to this device's own mailbox — the shape #140 fixed, where the members never receive it. Found by C1 while wiring the driver. The whole-pass engine selection therefore defaults to legacy, and the migration canary counts every group-addressed row as *unshadowed* rather than comparing it, so a clean report is never read as a claim about groups |
| What a contact endpoint that has gone silent costs an upload | two separate brakes: a *rejection* streak (the card is wrong) falls back to this device's own mailbox, while *silence* (no answer at all) declines to post at all, because falling back would put a cross-family contact's mail in a mailbox they never read and `relay_posted_at` is terminal | the same two brakes in `RelaySweepSession.swift` | **C3 — open.** `CoreRelayContactConfig` carries one `endpoint_usable` flag, so both brakes have to collapse onto it and the two answers cannot both be expressed: false means "ignore the card, use our own mailbox", which is the rejection answer applied to the silence case. C1 maps `usable && answering` onto the one flag and lets the canary report the difference as a destination or selection mismatch rather than hiding it; C3 owns the fix, and it is a second reason the core engine is not the default |
| Whether an unpostable recipient's rows consume a batch slot | no: `unpostableRecipients` is passed into the store query, so those rows are never selected and the batch is spent on rows that can actually move | no such exclusion; the rows are selected and skipped | **C3 — open.** `session/relay_pass.rs` passes an empty skip list to both upload queries, so under the core engine one unreachable contact can refill the batch every pass — the starvation the Android exclusion exists to prevent. The canary reports it as `shadow_selection_skip_differs` |
| Where the pending-rerun decision is made | explicit at the rerun point (`relayRerunAction`), which re-arms the coalesced retry timer for the remaining window | implicit: the pending nudge re-enters the front door, which drops it, and the retry armed when the 429 was recorded is what actually fires | B0 — resolved toward Android: both shells now call `core_relay_rerun_action` at the rerun point, so the deferral is a decision rather than a side effect of two gates agreeing |
| What seeds the anti-lockstep jitter | `ByteArray.contentHashCode()` — `java.util.Arrays.hashCode`, a 31-multiply over the user id | a hand-written FNV-1a over the user id, added because Swift's `hashValue` is process-randomized | B0 — resolved: neither. Core derives it from the public user id under a BLAKE2b context, and no shell computes a hash for this any more |

The three rows marked **C3 — open** are what package C1 gained by using this
table the same way. Nobody had recorded any of them as a difference, and all
three surfaced only because writing an adapter forced someone to answer what
the core engine would actually do with a group-addressed row, an endpoint
resting for silence, and a recipient this device cannot post to. Two of them
would lose or misroute mail if the engine selection defaulted to core today,
which is why it does not, and why the canary counts what it cannot speak for
rather than staying quiet about it.

The presence-scope row is the one this table gained by being used: nobody
recorded it as a difference, and it only surfaced because writing a single
implementation forced someone to answer it. That is the argument for keeping
the list append-only.

The first two were not known to be load-bearing when they were written down,
and that was the point of writing them down: an undocumented difference cannot
be reasoned about, and a migration that quietly changed Android's deployed
ordering to match a written stage enum would otherwise have landed with no
invariant, no fixture and no row saying it was ever different. Having to argue
each one out is what turned the first into a liveness question and the second
into a `SILENCE-01` question. Neither answer was obvious from the code.

All four resolutions are *decisions in core only* at this revision. C0 is
dark: no production path on either shell reaches `CoreRelayPass`, so nothing a
person is running has changed. The rows stay here rather than being deleted,
because the value of this table is that it records that a difference existed.

The jitter row was load-bearing in a small way and worth stating plainly. The
offset's *purpose* is that two phones in one family draw different values; two
shells drawing from different functions satisfied that by accident while
guaranteeing nothing, and no test on either side could have caught a change to
the other. It is one function now, with vectors both shells assert.

One difference that is deliberately **not** a divergence: the clock each shell
feeds the pacer. Android reads `SystemClock.elapsedRealtime()` and iOS reads
`DispatchTime.now()`; both are monotonic, which is the only property the pacer
requires, and choosing a monotonic source is exactly the kind of platform
decision the shells are supposed to keep.

## 6. Fixture and event schema — `cruisemesh.protocol-event/v1`

Fixtures live in `core/tests/fixtures/` as JSONL: one JSON object per line,
UTF-8, LF-terminated. The same schema serves three consumers — core replay
fixtures, simulation and decision-shadow transcripts, and the redacted
archive a person exports from Advanced diagnostics — so an exported archive
is accepted by the replay command with no conversion step.

The validator enforces schema, redaction, ordering, and declared invariant
ids. Since package C0 the relay-shaped fixtures are also **executed**: see 6.6
for what that means and for which two are not.

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
| `expect_invariants` | yes | every id must exist in section 1; non-empty for a fixture, and may be empty for a `redacted-field-archive` that recorded nothing invariant-tagged |
| `first_seq` | no | the sequence number of the first surviving record; absent means 1 |

`first_seq` exists because the live ring evicts. A device's sequence is its
own monotonic counter and is never renumbered on export: an archive that
silently restarted at 1 after eviction would read as a fresh phone rather than
as a phone that dropped its oldest evidence. Every checked-in fixture omits
the field and therefore means exactly what it did before the field existed.

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
| `inferred_at` | no | `true` when `at_ms` was borrowed from the previous record rather than measured; absent means the time was observed |
| `code` | yes | from the registry in 6.3 |
| `session`, `pass` | no | opaque short ids |
| `action` | no | non-negative integer |
| `actor` | no | must be a declared pseudonym |
| `invariants` | no | ids from section 1; each must also appear in the header's `expect_invariants` |
| `counts` | no | flat object, non-negative integers only |
| `outcome` | no | short stable token, never free prose |

### 6.3 Stable event codes

Codes are API. Prose log messages are not. The table is section 7, and
`core/src/protocol_event.rs` owns the list; a test fails if the two disagree.

### 6.4 Redaction rules

Every identity in a checked-in fixture is synthetic. Field-derived fixtures
carry archive-local pseudonyms, never a real user id — not even a hashed one,
because a hash of a stable id is still a stable id.

The closed key set is the structural half of this rule and applies to every
file the validator reads, not only to the checked-in corpus: a line carrying a
key that section 6.1 or 6.2 does not declare is rejected, so a leak cannot be
smuggled in under a field name nobody recognises. One list in
`core/src/protocol_event.rs` backs both the command and the corpus test, and a
test pins those tables against it.

The validator rejects a fixture that contains any of: a token prefix
(`cmdep1-`), a friend-card prefix (`CMFRIEND`), a deep-link scheme
(`cruisemesh://`), any `://` URL, an `Authorization` or `Bearer` header
fragment, a PEM key header, a private-address literal, or a JSON key outside
the schema. Payloads are represented by kind, length and digest — never
bytes.

### 6.5 Named incident fixtures

| Fixture | Incident | Declares | Executed |
|---|---|---|---|
| `sweep-livelock.jsonl` | a sweep that rescheduled forever without advancing (#270) | `PROGRESS-01`, `LIVE-01`, `CURSOR-01` | yes |
| `carry-storm.jsonl` | carried rows re-uploaded every launch for want of a marker (#222) | `MARK-01`, `CARRY-01`, `LIVE-01` | yes |
| `watermark-lock.jsonl` | receipt repair self-gated at a zero watermark (#241) | `WM-01`, `PROGRESS-01` | no — package D2 |
| `watchdog-spray.jsonl` | carried-first spray large enough to trip a watchdog (#280) | `SPRAY-01`, `LIVE-01` | no — package D2 |
| `429-mid-receipts.jsonl` | a family rate limit arriving mid-upload (#260, #261) | `RATE-01`, `LIVE-01` | yes |
| `short-page.jsonl` | a server-clamped page mistaken for EOF | `PAGE-01`, `CURSOR-01` | yes |
| `ack-fail-after-consume.jsonl` | durable consume, then a failed ack, then a restart | `TXN-01`, `CURSOR-01`, `IDEMP-01` | yes |
| `oversize-shrink.jsonl` | a page over the response cap, retried smaller | `PAGE-01`, `LIVE-01` | yes |
| `contact-silence-no-proof.jsonl` | a silent contact endpoint with no proof of own connectivity | `SILENCE-01` | yes |
| `pending-rerun-during-backoff.jsonl` | a pending nudge trying to start a pass inside the quiet window | `RATE-01`, `PROGRESS-01` | yes |
| `zombie-outbound-queue.jsonl` | an outbound queue that never retires anything (#283) | `QUEUE-01`, `LIVE-01` | yes |

### 6.6 What "executed" means, and what it deliberately does not mean

`core/tests/relay_pass_replay.rs` is the runner. For an executed fixture it
builds the scenario the fixture's title describes in a temporary
`MessageStore`, drives a real `CoreRelayPass` through it against a fake clock
and a scripted driver, and asserts the end state, the work counts, and that
every invariant the fixture declares held — in particular that the session
emitted no `invariant_violation` naming one of them.

It does **not** replay the fixture's event stream and require the session to
reproduce it, and that distinction is the whole point rather than a shortcut.
Most of this corpus is a transcript of a *bug*: `carry-storm`,
`sweep-livelock`, `watchdog-spray`, `watermark-lock` and
`zombie-outbound-queue` all contain `invariant_violation` records. A session
that reproduced them would be the incident happening again. So the fixture
stays the readable record of what went wrong and the index of which
invariants the scenario is about; the scenario is the executable proof that it
does not happen here. Outcome tokens and counts in a fixture are therefore not
a golden trace, and a session emitting different ones for the same scenario is
not by itself a disagreement.

Not a golden trace is not the same as unchecked. The counts a fixture states
that are *derived from a rule this repository owns* are checked against that
rule: a page-shrink step must be the one `relay_fetch_shrunk_limit` produces,
a frontier may not move backwards and a held one may not move at all, and a
page cannot consume or ack more rows than it returned. That is the class of
number a hand edit gets wrong, and it is how the one correction below was
found -- by reading, which is not a regression test, so the runner now carries
one.

Two fixtures are mesh-shaped and a relay pass cannot drive them at all: there
is no encounter, no peer link and no receipt-repair planner in one.
`watchdog-spray` and `watermark-lock` stay validate-only and are named to
package D2 (`mesh_meet`) in the runner's own scope list, which a test checks
against the fixture directory so a new fixture cannot be added without a
decision about whether it executes.

One fixture was corrected in the same commit as the runner, with the reason
recorded here because the plan requires it: `oversize-shrink` claimed a
256-row page was retried at 64 rows. `relay_fetch_shrunk_limit` halves, so the
retry is 128 and a second refusal is what reaches 64. The fixture had invented
a shrink step core's own rule does not produce, and it also carried a
`budget_yield` for a pass that respected its byte budget rather than yielding
on it. Both are fixed; the incident it describes is unchanged.

## 7. Event code table

Codes are API. A renamed code breaks the replay command, the fixture corpus,
and every archive already sitting in somebody's mail, so renaming one is a
contract change and adding one is not. `core/src/protocol_event.rs` owns the
list; a test in `core/tests/protocol_contract.rs` fails if this table and that
enum disagree.

Prose log lines remain non-API. They may be reworded freely, and nothing may
parse them.

The **emitter** column is the honest part. A code with no emitter yet is still
API: the fixture corpus uses several of them to describe incidents that
predate the ring, and the package that will emit each one is named here rather
than left to be discovered.

| Code | What it records | Emitter today | Typical invariants |
|---|---|---|---|
| `action_emitted` | an external request was handed to a driver | `session/relay_pass.rs` (dark) | `LIVE-01`, `RATE-01` |
| `action_result_accepted` | a driver's result was applied | `session/relay_pass.rs` (dark) | `IDEMP-01` |
| `action_result_stale_ignored` | a duplicate, late or wrong-pass result changed nothing | `session/relay_pass.rs` (dark) | `IDEMP-01` |
| `budget_yield` | a pass stopped inside a declared budget rather than at the end of its work | `session/relay_pass.rs` (dark) | `LIVE-01`, `PROGRESS-01` |
| `carried_row_marked` | a relay-uploaded carried row was durably marked | `session/relay_pass.rs` (dark), for the announce-time wholesale clear; C3 adds the per-row record | `MARK-01`, `CARRY-01` |
| `continuation_scheduled` | a pass scheduled more work, with its progress reason | `session/relay_pass.rs` (dark) | `PROGRESS-01` |
| `endpoint_recovered` | a contact endpoint answered again and its streak cleared | `store.rs` `clear_contact_relay_unreachable` | `SILENCE-01` |
| `endpoint_rested` | a no-answer streak reached the rest threshold | `store.rs` `note_contact_relay_unreachable` | `SILENCE-01` |
| `frontier_advanced` | a mailbox frontier moved forward over a fully processed page | `store.rs` `advance_relay_fetch_cursor` | `CURSOR-01` |
| `frontier_held` | a page moved neither the frontier nor the sweep cursor | `store.rs` cursor methods | `CURSOR-01`, `PAGE-01`, `PROGRESS-01` |
| `frontier_lowered` | a completed sweep proved the frontier sat above the top of the mailbox | `store.rs` `note_relay_sweep_completed` | `CURSOR-01` |
| `invariant_violation` | a named Contract v1 rule did not hold here | `store.rs` `note_invariant_violation`, and the ring's own redaction backstop | any |
| `outbound_queue_scanned` | the launch-time receipt-coverage sweep ran | `store.rs` `open` | `QUEUE-01` |
| `outbound_row_retired` | proof of delivery removed queued rows | `store.rs` `record_receipt` | `QUEUE-01`, `CARRY-01` |
| `outbound_row_superseded` | a newer generation of a snapshot kind replaced queued ones | `authoring.rs` `insert_authored_rows` | `QUEUE-01` |
| `page_ingested` | a relay page was durably consumed | `store.rs` `ingest_relay_page`, called by `session/relay_pass.rs` (dark) | `PAGE-01`, `TXN-01` |
| `pass_finish` | a relay pass ended, with its work counts | `session/relay_pass.rs` (dark) | `LIVE-01` |
| `pass_start` | a relay pass began, and why | `session/relay_pass.rs` (dark) | `LIVE-01` |
| `rate_limit_abort` | a family 429 ended the pass's remaining network work | `store.rs` `note_relay_rate_limit_abort`, called by the shells until package B0 owns the decision | `RATE-01`, `LIVE-01` |
| `receipt_watermark_observed` | a peer's receipt watermark was read during repair | none yet — package D2 | `WM-01` |
| `request_rejected` | an endpoint answered authoritatively that it would not serve us, or answered nothing at all | `store.rs` `note_contact_relay_rejected`; `session/relay_pass.rs` (dark) | `SILENCE-01`, `RATE-01` |
| `shadow_mismatch` | one sampled comparison between the live engine and the read-only migration planner: one summary record per sample, whether or not it found anything, then one record per disagreement | `store.rs` `note_relay_shadow_report`, called by Android's `RelayShadowAdapter` (C1) | any |
| `silence_observed` | contact silence was weighed at the end of a pass, with or without same-pass proof | `session/relay_pass.rs` (dark) | `SILENCE-01` |
| `spray_admitted` | a built spray plan went onto the radio | `spray_policy.rs` `admit_plan` | `SPRAY-01` |
| `spray_budget_exhausted` | a link ran out of burst allowance — once per dry spell, not per reconnect | `spray_policy.rs` `may_spray` | `SPRAY-01`, `LIVE-01` |
| `spray_deferred` | the receipt-quiet backoff took hold or deepened — on the crossing, not on every tick it holds | `spray_policy.rs` `may_spray` | `SPRAY-01` |
| `spray_planned` | a plan was built, before admission | none yet — package D2 | `SPRAY-01` |
| `spray_suppressed` | every lane advertised the set the peer was last offered | `spray_policy.rs` `admit_plan` | `SPRAY-01` |
| `sweep_completed` | a walk from 0 reached the empty page | `store.rs` `note_relay_sweep_completed` | `CURSOR-01`, `PROGRESS-01` |
| `sweep_restarted` | remembered sweep progress was thrown away, and why | `store.rs` `reset_relay_sweep_progress` | `PROGRESS-01` |
| `sweep_resumed` | a sweep already under way moved further up the mailbox | `store.rs` `advance_relay_sweep_cursor` | `CURSOR-01`, `PROGRESS-01` |
| `sweep_started` | a sweep's first page moved it off 0 | `store.rs` `advance_relay_sweep_cursor` | `CURSOR-01`, `PROGRESS-01` |

### 7.1 The ring, and what it costs

The ring is FIFO and capped at **both** 2,000 records and 1 MiB, evicting
oldest-first when either cap is reached. Two caps, because the records are not
one size: 2,000 spray decisions are small and 2,000 page ingests are not, and
an archive a family member is asked to send over ship wi-fi has a size budget
as well as a usefulness budget.

Granularity is a hard rule, not a preference. This store has an ANR history,
so an event is written per page, per pass, or per encounter decision — never
per envelope and never inside a hot loop. A delivered watermark that retires
200 rows is one record, not 200. A repeating condition is recorded when it
*changes* — entering the receipt-quiet backoff, deepening it, a link running
dry — never once per tick that it holds; a policy re-asked every minute would
otherwise replace the whole ring with one repeated non-event and evict the
evidence of the incident being investigated. Appends are batched, run against
a table that cannot exceed 2,000 rows, and never hold the store mutex across
anything that waits.

An append is atomic. Its rows, its evictions and its bookkeeping row land
together or not at all, inside a savepoint that nests under whatever
transaction the caller already has open — because rows committing while the
sequence counter does not would leave every later append failing its primary
key. The ring also reconciles its counter against the table it describes and
repairs a disagreement rather than jamming.

The ring is never the reason anything else fails. Every operational call site
emits best-effort: a full disk or a locked table costs a diagnostics record,
not a receipt, a page ingest, an authored message, or the store opening at
all.

Time in a record is clamped forward to the newest record already stored. Two
things make that necessary rather than defensive: a phone's wall clock can
step backwards mid-cruise, and a few decision points genuinely have no clock
in hand — `MessageStore::open` runs before anything has told core the time.
Such a record reads as "no earlier than the one before it", which is exactly
what is known, and it keeps "time never runs backwards" a property of the ring
rather than a hope about every caller. A record whose time was borrowed this
way says so with `inferred_at`, and the replay command reports those separately
and excludes them from a transcript's span: a borrowed timestamp read as a
measured one would tell a support reader that a frontier was held at a minute
nothing observed.

Export is manual. Nothing samples, schedules, or uploads it. The ring is
produced in full when a person taps share on the Advanced diagnostics screen,
and clearing captured diagnostics clears it.

### 7.2 The replay command

`cargo run -p cruisemesh-core --bin protocol-replay -- <file>` accepts a
checked-in fixture, a simulation transcript, or a diagnostics archive
straight out of the zip — one format, no conversion step.

It validates the schema — including the closed key set of 6.4, so it is no
weaker on a real archive than the corpus test is on a fixture — plus ordering
and redaction, checks every declared invariant id against section 1, walks the
transcript for the first place it contradicts itself (a pass that starts
twice, one that keeps working after its own rate-limit abort or after
finishing, a frontier that claims to advance without moving), and prints a
redacted summary.

It does not re-execute an *arbitrary* archive against a store, and it is worth
being exact about why that is not the same gap it was before C0. A field
archive is a redacted record of decisions, not the inputs that produced them:
it has no mailbox, no page bodies and no store to replay into. What executes
is the checked-in scenario corpus, in `core/tests/relay_pass_replay.rs`, where
the inputs exist. So a clean run of this command still means "nothing in this
file contradicts itself" — and the behaviour those files describe is now
separately proven by a suite that drives the real session. `--help` says
exactly this.

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
| Family request pacing and 429 backoff | `mesh/FamilyRelayBackpressure.kt` — delegating shim, no constants and no math | `Relay/FamilyRelayBackpressure.swift` — delegating shim, no constants and no math | `session/relay_policy.rs` owns the pacer, the exponential curve, its cap, the `Retry-After` floor and the stable identity jitter; `relay_status.rs` owns the header clamp and the classification | delete | done in B0; the shims survive only until B2's canary evidence permits deleting them |
| Pending relay rerun | `mesh/RelayRerunPolicy.kt` — delegating shim | `finishRelaySync` in `MeshController.swift` calls core at the rerun point | `session/relay_policy.rs` owns `core_relay_rerun_action`; `session/relay_pass.rs` refuses to start inside a quiet window and reports when it ends — **core (dark)** | delete | done in B0; the orchestration half is in `relay_pass.rs` and switches on in C1/C2 |
| Mailbox per-pass work/yield budget | none — `RelayMailboxWalkBudget.kt` was removed outright in #270; only the call-through `RelayMailboxWalkBudgetTest.kt` remains | none — `Relay/RelayMailboxWalk.swift` calls `relayMailboxWalkAction` directly and never had a local copy | `relay_cursor.rs` owns pages, envelopes, continuation delay (#270) | delete | already done in #270; nothing left to remove, and the Kotlin test stays as the guard that Android still reaches core |
| Mailbox walk execution | `mesh/RelayMailboxWalker.kt` (#276) | `Relay/RelayMailboxWalk.swift` (#276) | walk action, sweep due, resume, frontier in `relay_cursor.rs`; the walk itself is in `session/relay_pass.rs` — **core (dark)** | hoist-now | `relay_pass.rs`; C4 |
| Relay pass stage order | `mesh/RelaySyncEngine.kt` still runs every deployed pass; `mesh/CoreRelayPassRunner.kt` runs a core pass when the whole-pass engine selection says so, and that selection defaults to legacy (C1) | `relaySyncBlocking` region of `MeshController.swift`, unmigrated | `session/relay_pass.rs` owns the order as `CoreRelayStage`, and the 5.2 rows are decided there with reasons | hoist-now | `relay_pass.rs`; C2 extracts the iOS driver, C3–C5 move the remaining stages and delete the shells' copies |
| Relay HTTP execution and page-size cap | `relay/RelayClient.kt` for the legacy engine; `relay/CoreRelayDriver.kt` executes a typed action and infers nothing (C1). Both open their socket through one `RelayClient.openTransport`, so the network pin, the timeouts and the client's own transport headers cannot differ between engines | `Relay/RelayClient.swift`, unmigrated | codecs, caps and status classification in `relay_wire.rs` / `relay_status.rs`; `session/relay_pass.rs` forms complete requests (method, path, headers, body, declared max response bytes, and the response headers it wants back) and decodes the answers. `core_relay_adapter_vectors` is the shared table both adapters assert | hoist-now (semantics) / shell-forever (transport) | core request/response semantics, native execution; C2 |
| Sweep, frontier, ack, continuation | `RelaySyncEngine.kt` + `RelayMailboxWalker.kt` | `MeshController.swift` + `Relay/RelaySweepSession.swift` | `relay_cursor.rs` + `store.rs` helpers; frontier lowering is core (#279); `store.rs` `ingest_relay_page` is the transactional page ingest and `session/relay_pass.rs` orchestrates walk, ack, frontier and continuation — **core (dark)** | hoist-now | `relay_pass.rs` + the transactional store API; C4 makes the shells use it |
| Contact rejection / silence / rest | `mesh/ContactRelaySilence.kt` + engine | `ContactRelaySilence` in `RelaySweepSession.swift` + controller | `contact_relay_health.rs` + persisted store state; the same-pass proof gate is stage 8 of `session/relay_pass.rs` — **core (dark)** | hoist-now | policy in B0, orchestration in C3/C4 |
| Relay pass health fold | `mesh/RelayFaultPolicy.kt` — maps the core fold onto the shell's `RelayHealth` and adds the timestamp | `MeshConnectivityStatus.swift` — the same mapping | `session/relay_policy.rs` owns the worst-of fold and `CoreRelayPassHealth`; `relay_status.rs` owns the rank and the classification | presentation-only | done in B0; what is left on each shell is attaching a clock reading and choosing a display type, and D3 keeps it that way |
| Connection and delivery health classification | consumes core (#281, #282) | consumes core (#281, #282) | `connection_health.rs` owns classification, per-recipient delivery, receipt-gated lines | presentation-only | stays core; shells render |
| Failover resume debounce | `mesh/FailoverResumeDebounce.kt` — thin wrapper | equivalent wrapper | `transport_policy.rs` owns the window and coalescing (#269) | presentation-only | stays core; wrappers are adapters |
| Peripheral link admission and the post-reject spray cooldown | `mesh/PeripheralLinkAdmission.kt`, spray cooldown classes (#277) | not yet extracted | none — but the byte and cadence half of `SPRAY-01` is now core (#280), and this brake composes with it rather than duplicating it | hoist-later | `mesh_meet.rs` when D2 lands; what remains here is the notify-reject window itself |
| Per-encounter spray byte budgets and cadence | `mesh/SprayPolicy.kt` — thin delegate; the three constants are gone from `InboundEnvelopeProcessor.kt` | `Mesh/SprayPolicy.swift` — thin delegate; the three constants are gone from `Core/ProtocolKinds.swift` | `spray_policy.rs` owns the budgets, the per-link burst allowance, the cadence gate, per-lane identical-set suppression and the receipt-quiet backoff; `core_digest_spray_plan` reports each lane's advertised-set digest and byte cost, and the shells charge the lanes no plan can see | presentation-only | done (#280); stays core, and D2 absorbs the delegates rather than unwinding them |
| Inbound envelope disposition | `mesh/InboundEnvelopeProcessor.kt` | `processInboundEnvelope` and handlers in `MeshController.swift` | crypto/store primitives and ack eligibility in `engine.rs` / `store.rs` | hoist-now | `mesh_receive.rs`; D0/D1 |
| HELLO / digest / carry encounter | `MeshService.kt` + `InboundEnvelopeProcessor.kt` | `MeshController.swift` | digest and spray planning in `engine.rs`, session state in `transport_policy.rs`, and every spray decision (whether, how often, how much) in `spray_policy.rs` (#280) | hoist-now | `mesh_meet.rs`; D2/D3, which absorbs `spray_policy.rs` rather than re-deciding it |
| Logical peer routing | `MeshRouter.kt` / `MeshRouterState.kt` | `MeshRouter.swift` / `MeshRouterState.swift` | `CoreMeshRouterState` in `transport_policy.rs`; peer collapse is core (#266) | hoist-later | extend the existing core router; D2 |
| LAN endpoint cache and provenance | `mesh/LanEndpointCache.kt` | `LanEndpointStore.swift` | `lan_util.rs` owns provenance, eviction and same-network checks (#271, #278) | presentation-only | stays core |
| LAN endpoint hint authoring | `mesh/LanEndpointSender.kt` + `LanEndpointSendPolicy.kt` | full twin: `Mesh/LanEndpointSender.swift`, plus `sendLanEndpointHint` / `queueCurrentLanEndpoint` in `MeshController.swift` | encoder and host validation in `protocol.rs` | hoist-later | `ENDPOINT-01`'s authoring half; D2/D3 must move **both** copies |
| LAN scan and socket lifecycle | `LanTransport.kt` and scan files | `LanTransport.swift` and scan files | primitives in `lan_util.rs` / `lan_session.rs` | shell-forever (drivers) | shared progress policy in D2/D3 |
| BLE central / peripheral lifecycle | `BleCentral.kt`, `BlePeripheral.kt` | `BleTransport.swift` | framing only, in `framing.rs` | shell-forever | — |
| Push, OS polling, background wake | `relay/RelayPushClient.kt` + service scheduling | `Relay/RelayPushClient.swift` + controller scheduling | none | shell-forever | push stays a pass nudge only |
| Outbound queue retirement | no policy — `respondToDigest` in `MeshService.kt` calls the core re-seal | no policy — `handleDigest` in `MeshController.swift` calls the same | `outbound_retirement.rs` owns coverage retirement, supersession, per-kind expiry and whether a re-seal rejoins the queue; `store.rs` executes them at receipt time and on open (#283) | presentation-only | stays core; no shell decides any of it, and the digest responders' re-seal loop is the one caller that must stay a caller |
| Delivery / transport / health UI | Compose status surfaces | SwiftUI status surfaces | semantic facts in `connection_health.rs` / `semantic.rs` | presentation-only | core facts, native presentation; D3 |
| Field diagnostics archive | `debug/DiagnosticsShare.kt` + `debug/ProtocolEventExport.kt`, a wrapper that writes one file | `UI/DiagnosticsArchive.swift` + `ProtocolEventExport` in `UI/FieldMetricsExport.swift`, the same wrapper | `protocol_event.rs` owns the schema, the ring, redaction, export and replay; `store.rs`, `authoring.rs` and `spray_policy.rs` emit | presentation-only | done (B1); neither shell decides anything about an event, they attach a file to a share sheet |
| Relay migration canary | `mesh/RelayShadowAdapter.kt` captures a few legacy passes a day and compares them; it opens no socket, writes only the event ring, and refuses to run when the core engine is active (C1) | none — C2 decides whether iOS needs one | `session/relay_shadow.rs` is the read-only planner, and it calls `relay_pass.rs`'s own destination and request helpers rather than restating them | delete | removed with the legacy engine in C5; it is scaffolding for the evidence, not architecture |
| Multi-node orchestration test | n/a | n/a | `core/tests/mesh_sim.rs` reimplements receive and meet | delete | production `mesh_receive` / `mesh_meet`; D0/D2 |
| Generated bindings | ignored `kotlin-gen/` | checked-in `ios/CruiseMesh/Generated/` | `core/src/lib.rs` exports | shell-forever (mechanism) | drift is blocking in the Rust workflow (#269); keep it blocking |

### A.1 Rows that changed since the inventory was first drafted

Recorded so a reader does not have to diff two documents:

- Family request pacing, the 429 backoff curve, the stable jitter offset, the
  pending-rerun decision and the pass health fold are **core** now, in
  `core/src/session/relay_policy.rs` (package B0). Both shells kept their type
  names and every call site and became delegating shims with no constant and
  no arithmetic of their own. They are deliberately *not* deleted: deletion is
  B2's job and it is gated on paired-platform canary evidence, not on the
  hoist compiling. Three test suites — Rust, Android JVM and Swift XCTest —
  assert the same vectors, and the vectors cross UniFFI rather than living in
  a file each platform reads its own way, so a marshalling bug at either
  boundary shows up as a vector mismatch instead of as a platform test that
  quietly asserts something slightly different.
- The anti-lockstep jitter no longer comes from a platform hash on either
  shell. Section 5.2 records what each was doing and why neither survived.
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
  shell classes** (#277) with no core owner yet. They compose with the core
  cadence gate below rather than duplicating it: all three brakes are delays
  with finite expiries, and the core suite proves their composition cannot
  starve a peer.
- LAN endpoint hint authoring is **not** Android-only. iOS carries a full
  twin — `Mesh/LanEndpointSender.swift` for the kind-8 hint envelope, and
  `sendLanEndpointHint` in `MeshController.swift` for the `0x04` frame. D2/D3
  owns two copies, not one, and a hoist that moves only the Kotlin file
  leaves `ENDPOINT-01`'s authoring half exactly as split as it is today.
- The per-encounter spray byte budgets are **core constants** (#280), beside
  the per-link burst allowance, the cadence gate, per-lane identical-set
  suppression and the receipt-quiet backoff. Both shells deleted their copies
  and now receive budgets from core; each keeps a thin delegate
  (`SprayPolicy.kt`, `SprayPolicy.swift`) that maps keys, picks the monotonic
  clock and reports bytes queued, and decides nothing. What the shells still
  own is the *shape* of the encounter — which lanes run, and in which order —
  and one ordering there is load-bearing rather than cosmetic: the digest frame
  is enqueued ahead of every bulk lane, because core's exchange window is
  measured from that enqueue and a carried drain queued first would hold it in
  the FIFO past the window's own width.
- Stage 7 is not one order. Android walks the mailbox and then syncs
  presence; iOS syncs presence and then walks. Section 5.2 carries this and
  the presence-failure difference beside it.
- The relay pass itself is **core (dark)** as of package C0:
  `core/src/session/relay_pass.rs` owns the stage order, request formation,
  response decoding, response caps, store transactions, ack eligibility,
  cursor advancement, silence evidence, budgets and continuation, behind the
  one-action-at-a-time driver seam. Nothing on either shell reaches it — it is
  exported over UniFFI so the C1/C2 adapters have a surface to compile
  against, and its only caller in this repository is
  `core/tests/relay_pass_replay.rs`. The Appendix A rows above say `core
  (dark)` rather than `core` for exactly that reason: the decision has one
  home, and the two shells are still running their own.
- The three open 5.2 rows are **decided** (C0) and their reasons are recorded
  in that table: presence before the walk, a presence failure recorded rather
  than swallowed, and the quiet window committed at the refusal. C1/C2 migrate
  the shells onto those decisions, so no deployed behaviour changed in the
  package that made them.
- Outbound queue retirement is **core** (#283) and never was split: neither
  shell held a copy of it, so `outbound_retirement.rs` had nothing to hoist,
  only a gap to fill. Both shells got the smaller queue with no code change,
  because both read it through store calls that already existed.
- Android can now run a whole relay pass on `CoreRelayPass` (package C1). The
  selection is one value captured when a pass starts — legacy or core, never a
  mix of stages — it lives in this device's relay preferences rather than in
  the store so removing it later needs no migration, and it **defaults to
  legacy**. Three things landed with it: `relay/CoreRelayDriver.kt`, which
  executes one typed action and infers nothing; `mesh/CoreRelayPassRunner.kt`,
  which is the whole shell-side orchestration of a core pass and fits on a
  screen; and `mesh/RelayShadowAdapter.kt`, the read-only canary. Both engines
  open their sockets through one function, and `core_relay_adapter_vectors` is
  a shared table the Android suite asserts against requests recorded off a real
  server from both engines — so "the same bytes" is a comparison rather than
  two files agreeing.
- The canary is deliberately narrow and deliberately loud about it. It samples
  a bounded few legacy passes a day, captures the receipt and authored lanes as
  values, asks `session/relay_shadow.rs` what core would have planned, and
  records the disagreements in the event ring. It opens no socket — the types
  it is built from hold no endpoint, no connection and no callback, which a
  reflection test pins — writes nothing but diagnostics, and refuses to run at
  all when the core engine is the one moving mail. Every row it cannot speak
  for is counted, so a report of no disagreements never reads as a claim about
  rows nobody compared. It is removed with the legacy engine in C5.
- Three things must close before the Android default may move, and they are
  written down rather than discovered by flipping the switch. Two are the open
  5.2 rows above — group fan-out, and a contact endpoint resting for silence —
  and the third is that a page `CoreRelayPass` ingests is persisted but never
  handed to the shell's inbound processor, so nothing raises a notification for
  it. That third one is not a divergence between shells; it is simply the work
  package C4 exists to do, and it is named here so "the core engine is wired
  and tested" is not read as "the core engine is ready to be the default".
