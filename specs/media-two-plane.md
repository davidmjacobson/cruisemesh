# Media over two planes: design specification

Status: Proposed (rev 2) — the heart of the blob plane already exists as a
dark, fully-tested module tree at `core/src/media/` (manifest codec, chunk
crypto, bitmap, partial-transfer store, pull wire frames, both session state
machines), reachable by nothing and exported nowhere. Rev 2 documents that
concrete protocol, decides the three pieces rev 1 left open (link
multiplexing, the capability bit, the relayd blob API), and replaces the
delivery-phase sketch with implementation phases.
Platforms: Android and iOS, with relayd additions
Scope: photos at full quality, video clips, generic file attachments, and
the boundary that keeps all of them out of the delay-tolerant message
pipeline

## Outcome

A person can send a photo or a video clip in CruiseMesh and have it behave
the way the rest of the app behaves: the message never goes missing, the
bytes arrive as fast as the current network genuinely allows, and nothing
about it silently spends someone else's battery, storage, or money.

Concretely, after this design ships:

- Every recipient sees the message — sender, caption, thumbnail, size —
  immediately and unconditionally, over any path CruiseMesh has, including
  Bluetooth and carried delivery. The conversation is never missing an entry.
- Full-resolution bytes arrive opportunistically: within seconds when both
  phones share a ship or home network, when the recipient chooses to use the
  internet, or automatically later when a capable path appears.
- Photos stop being recompressed to fit the messaging pipeline. Video clips
  become possible at all.

## The one architectural rule

CruiseMesh gains a second data plane, and the two planes never mix:

| | Message plane (exists today) | Blob plane (new) |
|---|---|---|
| Carries | text, receipts, service kinds, attachment **manifests and thumbnails** | full-resolution photo and video **bytes** |
| Size per item | bounded by the sealed-envelope pipeline as today | bounded only by blob-plane policy |
| Transports | all of them: BLE, LAN, relay, carried hop-by-hop | bulk TCP paths only: LAN, relay blob store |
| Delivery model | push, store-and-forward, delay-tolerant | **pull**, by the recipient, resumable |
| Third-party phones | may carry and re-offer envelopes | **never** touch blob bytes |
| Failure behavior | waits indefinitely; that is the product | waits indefinitely too — but as a pending download, visible to its owner |

The message plane is universal and therefore expensive per byte: everything
in it is eligible to sit in another family member's carry queue, re-offer
against per-encounter spray budgets, occupy the family's shared relay
mailbox, and cross Bluetooth at single-digit KB/s. The blob plane is cheap
per byte and therefore not universal. Every design decision below follows
from refusing to blur that line.

## Problem

Attachments today are capped at a small blob size and ride the ordinary
envelope pipeline. The cap is not an aesthetic choice: a delay-tolerant mesh
spends *other people's* resources on every authored byte — courier storage on
family phones, per-encounter Bluetooth airtime, the family's shared relay
quota. The cap is what keeps a photo from monopolizing all three. The cost
is real, though: photos are visibly recompressed, and video is impossible.

Raising the cap would be the wrong fix. A single 50 MB clip in the universal
pipeline is over an hour of monopolized Bluetooth at realistic GATT
throughput, a meaningful fraction of the family's relay storage, and a
standing occupant of every courier's queue — the exact failure classes the
mesh's budget work exists to prevent.

## Non-goals

- **No real-time calls.** Store-and-forward cannot carry them and the
  product is not trying to (the voice recommendation is push-to-talk bursts
  on the existing attachment pipeline; that is a separate, smaller effort on
  shipped infrastructure).
- **No raise of the universal-pipeline attachment cap.** Small attachments
  and voice bursts continue to ride it unchanged; that is a feature, not a
  limitation — they work everywhere, including over Bluetooth and carry.
- **No blob bytes over BLE**, in any mode, including "just this once."
- **No third-party carry of blob bytes** in v1. A courier phone never
  stores, forwards, or serves another person's blob. (A consented
  mule-assist mode is listed under Future directions; it is explicitly not
  in scope now.)
- **No new wire protocol for the message plane.** Manifests use the already
  allocated attachment kinds; the message plane's frames do not change.
- **No automatic spending.** Blob transfer over a metered, roaming, or
  expensive path never starts without an explicit, size-aware user action —
  composing with the roaming-deferral policy rather than duplicating it.

## Message plane: the manifest is the message

Sending a photo or clip authors one ordinary attachment message containing:

- media type, byte size, dimensions/duration;
- the **content digest of the encrypted blob** (the blob's permanent name);
- the **blob key** (see Security), sealed to recipients like any message
  content;
- a thumbnail (photos: long edge bounded, visually useful; video: poster
  frame), sized so manifest + thumbnail together fit comfortably inside
  today's attachment envelope bound.

This message is delay-tolerant like any other: it carries, it mules, it
relays, it survives partitions, receipts cover it. Whatever happens to the
bytes, the *conversation* is complete on every device.

Rules:

- The thumbnail is mandatory for photos and video, generated at send time,
  and is the only degradation the message plane ever performs. There is no
  "full quality over the message plane" escape hatch. A generic file has no
  thumbnail; its bubble renders from the manifest's filename, type, and
  size instead.
- The manifest's digest names the encrypted bytes, so any copy fetched from
  anywhere can be verified before it is shown or stored.
- Deleting the local original after sending does not invalidate the
  manifest; it only limits which sources can still serve the blob.

## Blob plane: recipients pull, transfers resume

### Sources

A blob can be fetched from, in preference order:

1. **The sender's phone over LAN** — when both phones hold a live LAN link
   (the mesh already maintains authenticated TCP links on ship and home
   networks), the recipient requests chunks over a bulk sub-channel of that
   link. This is the primary path and the reason video is viable at all: a
   ship or home AP moves tens of megabytes in seconds to a minute.
2. **The relay blob store** — the sender may upload the encrypted blob to a
   new relayd blob endpoint (subject to consent and quota, below); any
   recipient with the manifest may then fetch it over the internet at their
   own pace.

Both sources serve the *same* encrypted, content-addressed bytes; a transfer
may begin against one source and finish against another.

### Chunking and resume (the "did we reinvent TCP" answer: no)

Within one TCP connection, ordering and reliability come from TCP. The blob
plane adds only what TCP cannot give across connections, days, and sources:

- The blob is divided into fixed-size chunks (LAN chunk size tuned for the
  link; relay fetches may use larger ranges). Chunk boundaries are derived
  from the digest-named blob, so they are identical at every source.
- The recipient persists a per-blob **chunk bitmap**. A resumed transfer —
  after an app restart, a network change, or switching source — requests
  exactly the missing ranges.
- On completion the assembled ciphertext is verified against the manifest
  digest before decryption; a mismatch discards the blob and re-requests it,
  and is a contract violation worth an event record if a *authenticated*
  source served it.
- All resume/scheduling policy (which source, which ranges, when to retry,
  when to defer) is pure core policy with table-driven tests. Platform code
  moves bytes.

### Consent and cost rules

- **LAN transfers auto-start** by default (they are free and local), bounded
  by a concurrent-transfer and bandwidth-courtesy policy so a burst of clips
  does not saturate the link the mesh itself is using. Auto-download on LAN
  is a per-device setting; default on.
- **Relay downloads and uploads over an expensive path never auto-start.**
  The recipient sees size before deciding. This composes with the
  roaming-deferral verdict: a roaming network defers blob transfer exactly
  as it defers relay sync, with the same Advanced override.
- **Relay upload is a sender decision** with the size shown, framed in user
  terms ("make this available over the internet"). On an ordinary unmetered
  network it may be a default-on convenience; on expensive paths it follows
  the consent rule.

### Quotas, expiry, and cleanup

- The relay blob store has a **per-family byte quota, separate from the
  mailbox quota**, enforced by relayd; the sender's app shows remaining
  space in Advanced. Blobs expire aggressively (days, not weeks) — the relay
  is a transfer window, not an album. Expiry of the relay copy never touches
  the manifest or anyone's completed download.
- On-device, received blobs live in the media store like any received
  photo/video (the user's own space); partially fetched chunk sets have a
  bounded budget and are garbage-collected oldest-first when it is exceeded.
- relayd enforces per-request range limits and the same family rate-limit
  discipline as the mailbox endpoints.

## Security and privacy

- **Blobs are encrypted before they are named.** The sender encrypts with a
  fresh per-blob key; the digest names the ciphertext; the key travels only
  inside the sealed manifest. The relay and any future assisting party store
  and serve bytes they cannot read — the same posture as sealed envelopes.
- One blob, many recipients: a group send seals the same blob key to each
  recipient's manifest copy; the ciphertext uploads once and is fetched per
  recipient.
- **The endpoint-privacy invariant is untouched.** A recipient fetches over
  LAN only from a link the mesh already authenticated with the sender, and
  otherwise from the relay. Nothing about the blob plane discovers, stores,
  or forwards any third party's address.
- Blob fetch requests carry no more identity than the mailbox protocol
  already does; possession of a manifest (digest + sealed key) is the
  capability to fetch and read.

## Wire protocol

What follows is the protocol as implemented in the dark module (normative
where it exists — the code and its tests are the reference), plus the three
decisions rev 1 deferred, marked **(new in rev 2)**: how pull frames share
the LAN link, how support is advertised, and the relayd blob API.

### Message plane: the manifest message

A media send authors one ordinary sealed message of kind
`KIND_ATTACHMENT_MANIFEST` (16, allocated since the media-readiness work;
kind 17 `KIND_ATTACHMENT_CHUNK` stays reserved and unused — chunks never
ride the message plane). The body is the versioned manifest codec in
`core/src/media/manifest.rs` (`MANIFEST_WIRE_VERSION = 1`):

| Field | Meaning |
|---|---|
| `blob_id` (32 bytes) | BLAKE2b-256 of the ciphertext — the blob's permanent name |
| `blob_key` (32 bytes) | per-blob key; travels only here, inside sealed content |
| `ciphertext_bytes`, `plaintext_bytes` | exact sizes, so geometry is derivable everywhere |
| `kind` | photo (1), video (2), or file (3, new in rev 2) |
| `mime` (≤128 B), width/height, `duration_ms` | render metadata |
| `filename` (≤255 B, new in rev 2) | the sender's display name for the item; required for kind file, optional otherwise |
| `thumbnail` (≤64 KiB) | mandatory for photo/video (poster frame for video); absent for file |
| `caption` (≤4 KiB) | ordinary text |

The encoded manifest is capped at 96 KiB (`MEDIA_MANIFEST_MAX_BYTES`), and a
test pins that a maximal manifest fits today's attachment envelope with
room. The decoder permits trailing extension bytes, so the manifest can grow
without a version bump.

### Blob encoding: encrypt, then name

`core/src/media/blob.rs`. The plaintext is split into 256 KiB chunks
(`MEDIA_CHUNK_PLAINTEXT_BYTES`); each chunk is sealed independently with
XChaCha20-Poly1305 under the per-blob key (AAD domain
`cruisemesh.media.blob/v1`, nonce prefix `cmblob01` + chunk index), so every
chunk but the last is exactly 256 KiB + 16 bytes of ciphertext and a chunk's
ciphertext offset is a multiplication, not a lookup — chunk boundaries are
identical at every source. `blob_id` = BLAKE2b-256 over the whole
ciphertext, verified on completion (`verify_assembled`) and per-chunk on
arrival (a chunk that fails its AEAD open is rejected and stays missing in
the bitmap — BLOB-05 at chunk granularity). Blob cap: 128 MiB
(`MEDIA_BLOB_MAX_BYTES`).

### Generic files (new in rev 2)

Everything below the manifest is content-agnostic — chunking, chunk crypto,
the pull proof, resume bitmaps, session budgets, the relay blob store, and
every BLOB invariant apply to a PDF exactly as they apply to a clip. So
generic file attachments are a manifest-kind, not a new mechanism:

- **Wire.** `kind = 3 (file)` plus the `filename` field, both added to
  manifest wire v1 *now*, while the codec is dark and nothing has shipped —
  after Phase 2 ships, additions ride the codec's trailing-extension room
  instead. Width/height/duration are zero for files; the same 128 MiB blob
  cap applies.
- **Receive side.** A completed, verified file lands in the platform's
  document space (Downloads / the Files app), never the photo library, via
  the platform's own save mechanism. The manifest filename is display
  metadata, not a path: it is sanitized before any filesystem use (no
  separators, no traversal, no leading dots), collisions get a numeric
  suffix, and the saved file's extension is derived from the manifest mime
  rather than trusted from the name. CruiseMesh never auto-opens a received
  file; the bubble offers open-with/share through the platform sheet.
- **Consent.** Files follow the same cost rules as media — auto-fetch on
  LAN, size-aware consent on expensive paths. No special case.

### Capability advertisement (new in rev 2)

Allocate HELLO2 capability bit `CAP_MEDIA_BLOB = 1 << 5` in
`core/src/protocol.rs`, OR'd into `core_own_capabilities()`. It means: this
device renders media manifests and speaks the LAN pull sub-channel (both
roles). It gates nothing on the message plane — a manifest is delay-tolerant
mail like any other and is sent regardless — but a requester only opens a
pull session against a peer that advertised the bit, and the sender's UI may
use its absence to explain a contact stuck on an old build. Legacy HELLO
never grows trailing fields; the bit rides frame 0x06 only.

### LAN sub-channel: multiplexing (new in rev 2)

The pull conversation rides the *existing* authenticated LAN link — the
Noise XX session of `core/src/lan_session.rs`, whose encrypted records
already carry a one-byte record type. Today `RECORD_TYPE_FRAME = 1` is the
only allocation. Rev 2 allocates:

- **`RECORD_TYPE_BLOB = 2`** — a record whose reassembled payload is one
  encoded `PullFrame` (`core/src/media/wire.rs`), in either direction.

Blob records use the same 9-byte inner header (`record_type ‖ frame_id:u32 ‖
index:u16 ‖ total:u16`) but their `frame_id` space is independent of record
type 1's, and each record type gets its own single-in-flight reassembler.
Records of the two types may interleave freely on the wire: a 256 KiB chunk
frame spans ~5 Noise records of ≤60 KiB, and mesh-plane records may be
emitted between them. That interleaving point *is* the courtesy mechanism —
the acceptance criterion that a transfer in progress never measurably delays
the message plane is tested at this layer (mesh records get priority at the
send queue). A peer that did not advertise `CAP_MEDIA_BLOB` treats record
type 2 as it treats any unknown type today: reject the record, keep the
link. Rejected alternative: a second TCP connection with its own handshake —
more sockets, a second discovery/firewall story, and nothing the record mux
doesn't already give.

### LAN pull sub-channel: frames and proof

`core/src/media/wire.rs` and `core/src/media/lan_pull.rs`, already
implemented and table-tested. Frame tags (their own byte space, inside
record type 2): `Open(1)`, `Challenge(2)`, `Fetch(3)`, `Chunk(4)`,
`BatchDone(5)`, `Refused(6)`, `Close(7)`.

```text
requester                              responder
---------                              ---------
Open{blob_id}            ->
                         <-            Challenge{nonce, chunks_held}
Fetch{proof, ranges≤8}   ->
                         <-            Chunk{index, ciphertext} …
                         <-            BatchDone
Fetch{…}                 ->            (window: ≤16 chunks / 4 MiB)
…                        ->            Close
```

A responder answers only for blobs it holds and only to a requester that
proves manifest possession: `proof = BLAKE2b-256("cruisemesh.media.pull-proof/v1"
‖ blob_id ‖ nonce ‖ blob_key)`, nonce chosen by the responder, single-use.
This is abuse resistance (bandwidth, existence-probing, conversation-scoped
consent), not confidentiality — the ciphertext is unreadable without the
sealed key regardless. Refusals (`NotHeld`, `ProofInvalid`, `BadRequest`,
`BudgetSpent`, `Busy`) are terminal for the session; `BudgetSpent` is a
pause, and the requester resumes later from its bitmap.

Both roles run as pure state machines (typed actions out, typed results in,
explicit `now_ms` — the `relay_pass.rs` shape) with declared budgets: a
requester session issues ≤64 fetches and accepts ≤48 MiB inside a 60 s
deadline; a responder serves ≤512 chunks / ≤48 MiB / ≤64 fetches. A 128 MiB
clip deliberately spans several sessions; the bitmap makes each next one
cheap.

### Resume state

`core/src/media/bitmap.rs` + `core/src/media/store.rs`: a persisted per-blob
chunk bitmap with a `missing_ranges` walk, and SQLite metadata
(`media_blobs` table, applied to the app's existing `MessageStore`
connection at integration) tracking bytes present, verification state, and
LRU order for the 512 MiB partial-transfer GC budget. A completed, verified
blob leaves for the platform media store and is no longer charged.

### Relay blob store API (new in rev 2, Phase 2)

relayd gains a content-addressed blob store beside the mailbox, same bearer
auth, same limiter discipline:

| Route | Method | Token class | What it does |
|---|---|---|---|
| `/blobs/{blob_id}` | `POST` | member | Create: declares `ciphertext_bytes`; checked against the family blob quota; 409 if complete copy exists (dedupe — success for the sender) |
| `/blobs/{blob_id}` | `PATCH` | member | Ranged upload: `Content-Range` ciphertext bytes, resumable; on the final range relayd verifies BLAKE2b-256(ciphertext) = `blob_id` and only then marks the blob fetchable — a digest mismatch discards the upload |
| `/blobs/{blob_id}` | `HEAD` | member or deposit | Existence + completeness + size, so a recipient can price the download before consenting |
| `/blobs/{blob_id}` | `GET` | member or deposit | Ranged fetch, ≤4 MiB per request (`Range` header), bytes charged to the requesting token's byte bucket |

Auth for cross-family recipients is the deposit token they already hold: a
friend card carries the sender family's post-only deposit token, and Phase 2
widens the deposit class from "may post mail" to "may post mail and fetch
this family's blobs" — read access to unreadable ciphertext, gated on
possessing both the card and the 32-byte blob id from a sealed manifest.
Deposit tokens still cannot touch envelopes, presence, or WS. Rejected
alternative: unauthenticated fetch-by-blob-id — an unguessable name is a
capability, but an unauthenticated 128 MiB endpoint invites scraping and
sits outside the per-family limiter that protects the hosted service.

Storage: blob bytes as files on disk under the relayd data dir, one metadata
row per blob in SQLite (family token, sizes, completeness, `expires_at`,
per-blob upload bitmap); never inline in the mailbox database. Quota is a
separate per-family blob quota (512 MiB default, `families` column beside
the mailbox quota, enforced at create and at upload commit). Expiry is 7
days from completion (incomplete uploads expire faster), pruned by the
existing sweep. All four routes ride the family rate limiter's request and
byte dimensions plus the global backstop.

## Contract invariants

All six are already registered — in `specs/protocol-contract-v1.md` §1 and
the machine index `core/tests/protocol_contract.rs`. BLOB-02, BLOB-04, and
BLOB-05 are core-owned today (the dark module's tests); BLOB-01, BLOB-03,
and BLOB-06 are registered `unimplemented` and flip to executable owners in
the phases below.

- **BLOB-01 — plane separation.** Blob bytes never enter an envelope, a
  carry queue, a digest spray plan, or any BLE frame. (The existing spray
  and carry tests gain blob-flavored adversarial cases.)
- **BLOB-02 — ciphertext addressing.** The wire and the relay only ever see
  encrypted bytes; the blob key exists only inside sealed message content;
  digests name ciphertext.
- **BLOB-03 — pull with consent.** No blob transfer starts on an expensive
  or roaming path without an explicit user action; no third party is ever
  asked to move blob bytes.
- **BLOB-04 — bounded everywhere.** Device partial-chunk budget, relay
  family quota, relay expiry, and per-request range limits are all enforced
  and tested; a blob transfer terminates or defers inside declared budgets
  like every other pass.
- **BLOB-05 — verify before trust.** No blob is decrypted, shown, or
  retained without matching its manifest digest.
- **BLOB-06 — the relay blob store is a separate, bounded window.** Its
  per-family quota is distinct from the mailbox quota, its copies expire in
  days, and its ranged fetches sit under the family rate limiter — owned by
  relayd e2e tests when Phase 2 ships.

## UX requirements (family-obvious surface)

- A media message always renders instantly as thumbnail + size; the bubble
  is never blank and never blocks the conversation. A file message renders
  the same way from filename + type + size, with a document glyph where the
  thumbnail would be.
- Pending state is calm and truthful, reusing the connection-details
  vocabulary: "Will download when you're near Dad" / "Waiting for internet" /
  "Tap to download over the internet (34 MB)". Never a warning color for
  ordinary waiting.
- Progress and pause/resume on the bubble for large transfers; a failed
  chunk set retries quietly, and a verify failure reads as a plain retryable
  error, not jargon.
- Defaults: auto-download on LAN and unmetered Wi-Fi; ask first on relay
  when the path is expensive; per-chat override in the chat's settings;
  everything else behind Advanced.
- Sender-side: video capture/pick offers a clip-length guideline rather
  than a hard tiny cap, with the real bound coming from blob-plane policy;
  the size is always shown before send when the relay upload consent
  applies.

## Sizing (initial numbers, tunable from field evidence)

| Parameter | Initial value | Rationale |
|---|---|---|
| Thumbnail budget | fits manifest + thumbnail in today's attachment envelope | keeps the message plane unchanged |
| Blob size cap (v1) | 128 MB | covers phone video clips; not a movie service |
| LAN chunk | 256 KiB | small enough to interleave with mesh traffic on the same link |
| Relay range cap | 4 MiB per request | plays fair with mailbox endpoints under the family limiter |
| Relay blob quota | 512 MiB per family, separate from mailbox | a transfer window, not storage |
| Relay blob expiry | 7 days | matches the delivery-window philosophy |
| Partial-chunk GC budget (device) | 512 MiB, oldest first | bounded incompleteness |

## Implementation phases

Phase 0 — the pure protocol core — is done: `core/src/media/` implements
everything above except the two "(new in rev 2)" link/relay pieces, dark and
table-tested. What remains is wiring, in shippable slices. Each phase is one
PR series off its own branch, lands green on the full workspace, and ships
both platforms together where behavior is shared.

**Phase 1 — core surface and link plumbing (pure Rust, no UI).**
The whole of this phase is testable with `cargo test` alone.

- `RECORD_TYPE_BLOB = 2` in `lan_session.rs`: per-type reassemblers,
  independent frame-id spaces, mesh-priority interleaving at the record
  layer, unknown-type behavior pinned by test.
- `CAP_MEDIA_BLOB = 1 << 5` in `protocol.rs` + `core_own_capabilities()`,
  with the bit-allocation test pattern the existing bits use.
- The manifest codec's rev-2 additions while the wire is still dark:
  `kind = file (3)` and the `filename` field, with filename-sanitization
  policy as pure, table-tested core logic.
- The integration module the checklist in `core/src/media/mod.rs` owes:
  authoring (seal + manifest encode as a `KIND_ATTACHMENT_MANIFEST` body),
  receive-side manifest recognition opening a `BlobStore` row,
  `MEDIA_SCHEMA_SQL` applied on the `MessageStore` connection (and the
  backup posture for partial transfers decided: metadata backs up, chunk
  files do not), and the blob-transfer consent verdict composed from
  `core_relay_network_permitted` rather than duplicated (LAN is always
  permitted; relay paths inherit the roaming/constrained verdict).
- UniFFI exports for all of the above; regenerate bindings.
- Adversarial BLOB-01 cases in the spray and carry suites; flip BLOB-01 and
  BLOB-03 to core-owned in the protocol contract.

**Phase 2 — photos end-to-end on LAN, both shells, one PR series.**
The first user-visible slice: full-quality photos when the phones share a
network.

- Send path: photo pick, thumbnail generation, manifest authoring, original
  retained as the servable blob.
- The LAN sub-channel drivers on both shells: socket writes for record type
  2, chunk-file writes, `take_accepted` drain, serve-side chunk reads.
- Bubble UX: instant thumbnail + size, pending states in the
  connection-details vocabulary, progress, auto-download-on-LAN setting
  (default on). All copy through resources.
- Acceptance: the LAN-media criteria below, applied to photos, including
  the kill-and-resume test and the message-plane-latency courtesy bound.

**Phase 3 — video, generic files, resilience, field soak.**

- Video capture/pick with the clip-length guideline, poster-frame
  thumbnails, duration metadata.
- Generic files: document pick on send, the file bubble, save-to-Downloads
  with sanitized names and the platform open-with sheet on receive. Same
  transfer machinery as photos — this is UX plus the receive-side save
  path, both shells.
- Multi-session transfers exercised for real (a 128 MiB clip spans ≥3
  sessions), partial-GC behavior verified, source re-selection after
  network changes.
- Two-phone rig smoke extension for the blob plane; run the full acceptance
  list on hardware.

**Phase 4 — relay blob store (works apart, not just together).**

- relayd: the four `/blobs/{blob_id}` routes, disk-file storage, separate
  family blob quota, expiry sweep, deposit-class widening, e2e tests; flip
  BLOB-06 to owned.
- Clients: sender "make this available over the internet" consent with size
  shown, recipient size-aware download, roaming-deferral composition,
  remaining-space in Advanced.

**Phase 5 — polish and study.** Group-send efficiencies, per-chat policies
informed by field metrics, and a *study* (not a commitment) of consented
mule-assist for blobs — whether a plugged-in, opted-in family device may
courier encrypted blobs under its own storage grant.

## Sequencing

This is deliberately a new plane rather than a change to the existing one: no
policy here duplicates anything the in-flight mesh/session consolidation is
unifying, and none of it should land inside that work. Build it after the
peer-encounter half of that consolidation settles, since its budgets are the
ones BLOB-01 must respect. The core policy module (chunking, bitmap, pull
sessions — pure and table-tested) is already landed dark; the implementation
phases above sequence the rest: core wiring and link plumbing first, then
the LAN bulk sub-channel drivers, then relayd. Voice
push-to-talk does not wait for any of this; it rides the existing pipeline
today.

## Acceptance criteria (LAN media — met across Phases 2 and 3)

- A 30-second clip sent between two phones on one Wi-Fi network completes
  in well under a minute and survives an app kill + relaunch mid-transfer,
  resuming from the bitmap rather than restarting.
- The same send with the phones apart shows thumbnail + pending state on
  the recipient immediately via BLE/carry/relay, and auto-completes on the
  next shared network without user action.
- A blob transfer in progress never increases message-plane latency
  measurably (the courtesy bound is tested, not hoped).
- Zero blob bytes observable in any envelope, spray plan, carry queue, or
  BLE frame under an adversarial test that sends media continuously.
- Digest-verification failure discards, retries, and never renders.
- All copy through resources; the localization gate passes; the pending
  states reuse the connection-details vocabulary.
