# Adversarial payload review

Review date: 2026-07-19  
Base: `master` at `8654912`  
Scope: malformed or malicious data reaching the Rust core, BLE and LAN link
framing, relay clients and `relayd`, attachment/content decoding, persistence,
the Noise pre-authentication surface, and UniFFI callers.

## Executive summary

No memory-safety or plaintext-forgery issue was found. The cryptographic open
paths fail closed, the hand-written Rust cursors bounds-check before slicing,
LAN packet lengths are checked before allocation, and LAN application frames
do not flow until Noise authenticates an accepted contact.

The main risk is denial of service. BLE permits a roughly 33 MiB reassembled
frame even though relay envelopes are capped at 512 KiB. Once parsing fails to
open an envelope, the DTN path intentionally stores opaque data; an attacker
who is an accepted contact can make that storage unbounded by choosing the
contact's own recipient hint, unique public message IDs, and far-future
expiries. The public routing header is mutable and is not bound to the sealed
payload, so copying one ciphertext under fresh message IDs also defeats the
current carry-queue dedupe.

The fixes below should be separate reviewable changes. The first four are the
recommended release gate.

## Threat model

- A nearby, unauthenticated BLE peer can connect and send link frames.
- A LAN peer can reach the TCP listener, but application frames require a
  completed Noise XX handshake whose static key maps to an accepted contact.
- An accepted contact may be malicious or compromised.
- A configured relay or a client holding a valid family token may be malicious
  or compromised.
- Backup files and shared friend-card text are untrusted inputs selected by a
  user.
- Denial of service includes process crashes, memory pressure, persistent disk
  exhaustion, and forcing expensive work before authentication.

## Findings

### T4-01 — High — Family carry storage can be exhausted indefinitely

Affected paths:

- `MeshService.processInboundEnvelope` / `carryForeignEnvelope`
- `MeshController.processInboundEnvelope` / `carryForeign`
- `MessageStore.enqueue_carried_envelope`
- public envelope fields decoded by `protocol::parse_frame`

The opaque carry path runs after pairwise and group opening fail, as intended
for DTN forwarding. `recipient_hint` alone decides whether the row is family
traffic. Family rows are expressly exempt from the 5 MiB foreign budget, are
deduped only by attacker-selected `msg_id`, and accept the attacker-selected
`expiry`. The public `msg_id`, `hop_ttl`, `expiry`, and `recipient_hint` are not
cryptographically bound to `sealed`.

Reproduction:

1. Become an accepted contact of the target phone.
2. Compute today's recipient hint for that contact identity.
3. Send envelopes that cannot open on the target, using that hint, a new
   16-byte `msg_id` each time, and `expiry = i64::MAX`.
4. Every envelope is classified as family and inserted into
   `carried_envelopes`; no family budget or content-level duplicate check
   removes it. Rewrapping the same `sealed` bytes under new IDs is sufficient.

Impact: persistent disk exhaustion and repeated relay/flood work. Before the
frame-size fix below, one BLE envelope can also be tens of MiB.

Fix:

- Validate a maximum future expiry and maximum hop count at every ingress.
- Apply a hard total carry-queue budget while retaining family-first eviction.
- Dedupe carried content using a digest of `recipient_hint || sealed` in
  addition to `msg_id`; including the hint preserves legitimate per-member
  group fan-out rows that reuse one group ciphertext.
- Keep the DTN safety invariant: eviction must never be mistaken for delivery
  or generate a relay ack.

Planned branch: `agent/t4-carry-queue-hardening`.

### T4-02 — High — BLE reassembly allows a pre-authentication memory DoS

Affected path: `core/src/framing.rs::BleFrameReassembler::accept`.

The 16-bit fragment count permits 65,535 fragments. At the 512-byte ATT value
ceiling, the four-byte header leaves 508 bytes per fragment, so one frame can
grow to roughly 33.3 MiB. Reassembly happens before `parse_frame`, decryption,
or sender authorization. Parsing and cryptographic opening create additional
copies. LAN caps a decrypted frame at 1 MiB and relayd caps `sealed` at
512 KiB, so BLE is the outlier.

Reproduction: feed one `BleFrameReassembler` ordered fragments numbered
0 through 65,534, each declaring total 65,535 and carrying 508 payload bytes.
The final `accept` returns the full roughly 33.3 MiB frame instead of dropping
it. The same object backs Android and iOS BLE reception.

Impact: a nearby unauthenticated device can pressure memory until the native
app is killed, while also monopolizing BLE receive time.

Fix: define one shared P2P frame/envelope ceiling derived from relayd's
512 KiB sealed limit, reject oversize fragments and cumulative reassembly,
clear partial state on rejection, and make `parse_frame` enforce the same
ceiling as defense in depth.

Planned branch: `agent/t4-p2p-frame-limits`.

### T4-03 — Medium-high — Relay responses are materialized without bounds

Affected paths:

- `core/src/relay_wire.rs::relay_decode_fetch_page`
- Android `RelayClient.useJsonResponse`
- iOS `RelayClient.syncRequest`

Both clients read the entire HTTP body before calling the core. The core then
deserializes arbitrary JSON and base64-decodes every envelope without a body,
row-count, per-envelope, or aggregate decoded-byte limit. A compromised relay
can return an arbitrarily large response. Even an honest maximum-size page is
large: the clients request 128 rows and relayd permits 512 KiB sealed per row,
which is 64 MiB decoded and about 85 MiB of base64 before JSON overhead and
copies.

Reproduction: configure a test relay endpoint that answers a fetch with one
very large base64 `sealed` string, or 128 near-limit rows. Android's `readText`
and iOS `URLSession.dataTask` materialize the response, followed by another
JSON/base64 allocation in Rust.

Impact: remote memory exhaustion from the configured relay and excessive
memory use during a legitimate attachment backlog.

Fix: reduce fetch batch size, bound response bytes while reading, and enforce
matching body/row/per-envelope/aggregate limits in `relay_wire` before and
during decode. Validate `next_cursor` and row IDs as non-negative and
monotonic as a secondary protocol check.

Planned branch: `agent/t4-relay-response-limits`.

### T4-04 — Medium-high — Unknown senders can persist chat and group state

Affected paths:

- Android `deliverOpenedEnvelope`, `handleIncomingChatMessage`, and
  `handleIncomingGroupInvite`
- iOS `deliverOpened`, `handleIncomingChat`, and
  `handleIncomingGroupInvite`

Successful `open_message` authenticates the signing identity, but it does not
authorize that identity as a contact. Text, attachment, and reaction handlers
insert the message before looking up the contact. Group invites require only
that the self-signed sender and recipient appear in the supplied member list;
they do not require the inviter to be an accepted contact. Friend requests and
ticket-authorized introduced requests legitimately need an unknown-sender
exception, but ordinary chat and group invites do not.

Reproduction:

1. Obtain the recipient's public agreement key from a shared friend card.
2. Generate an arbitrary signing identity and seal a valid text body to the
   recipient, with `chat_id` equal to the generated sender ID.
3. The message is inserted on both platforms before contact lookup. A
   similarly sealed group invite imports attacker-chosen group state and can
   produce a notification.

Impact: persistent spam and storage growth from identities the user never
accepted. The data is signed, but it is not authorized.

Fix: add a core-owned pairwise kind authorization decision. Require a known
contact for chat, receipts, profile/directory data, LAN hints, and group
invites; retain explicit exceptions only for direct friend requests and
cryptographically ticketed introduced requests. Both shells must gate before
any message/group write.

Planned branch: `agent/t4-inbound-sender-authorization`.

### T4-05 — Medium — Group and identity structures permit amplification

Affected paths:

- `groups::decode_group_invite_content` and
  `decode_group_metadata_update`
- `groups::validate_group` / `validate_group_metadata_update`
- `protocol::decode_profile_sync_content`
- `identity::parse_friend_card`

A group payload can declare up to 65,535 members, and member IDs may be empty
or arbitrary length. Within a 512 KiB envelope, roughly 32,000 empty members
can reach canonicalization and SQLite group-member writes. Group names,
profile names, and direct friend-card strings also lack product-scale limits
(their wire formats allow tens or hundreds of KiB).

Reproduction: construct a valid, signed group invite containing a member
count in the tens of thousands with tiny IDs, while including the sender and
recipient IDs. An accepted sender reaches `upsert_group`, which performs one
SQLite insert per canonical member.

Impact: long receive-thread stalls, oversized UI strings, and persistent row
amplification from one bounded envelope.

Fix: enforce 16-byte user IDs, a family-scale member-count limit, and short
UTF-8 name/card field limits in the Rust decoders and constructors before
shell code or SQLite sees the values.

Planned branch: `agent/t4-structured-content-limits`.

### T4-06 — Medium — iOS can mark failed delivery consumed and ack it

Affected path: `MeshController` inbound handlers and
`processInboundEnvelope`.

Several iOS handlers use `try?` around store operations and collapse a store
error into `false`, the same value used for a harmless duplicate. The outer
function then records the public message ID as seen and returns `.consumed`.
Relay polling can consequently ack and delete the relay's only copy even
though a disk-full or corrupt-store failure prevented durable delivery.
Android generally lets those store errors escape, which preserves
re-presentability but can terminate a callback thread because the top-level
frame handler does not consistently translate storage errors.

Reproduction: make the SQLite store unwritable/full, then deliver a valid new
pairwise message through relay polling. `insertIncomingMessage` fails, the
iOS handler returns as if no row was inserted, and the outer path reports
consumed.

Impact: message loss under resource pressure, with resource exhaustion making
the condition attacker-influenceable.

Fix: return an explicit `stored`, `duplicate`, `rejected`, or `failed`
delivery result. Record seen/ack only for durable storage or a proven durable
duplicate; leave failures re-presentable. Keep the decision in the core and
make both shells handle store errors without crashing.

Planned branch: `agent/t4-durable-ingest-result`.

### T4-07 — Medium — Backup KDF parameters allow a pre-auth CPU bomb

Affected paths: `backup::open_backup` and both platforms' whole-file readers.

The backup header supplies a `u32` PBKDF2 iteration count. Only zero is
rejected; `0xffff_ffff` is accepted and used before the AEAD tag can be
verified. A crafted file therefore needs no valid passphrase or ciphertext to
force billions of SHA-256 iterations. Android `readBytes` and iOS
`Data(contentsOf:)` also load a selected file without a maximum size before
calling the core.

Reproduction: create the minimum-length backup header with the correct magic,
version, KDF ID, and iteration bytes `ff ff ff ff`, append a 16-byte dummy GCM
tag, and attempt restore. The KDF runs before authentication fails.

Impact: user-triggered app hang and battery/thermal exhaustion from a shared
malicious backup file.

Fix: accept a narrow supported KDF iteration range, reject oversized files in
the platform readers before allocation where possible, and enforce a core
file/payload ceiling as defense in depth.

Planned branch: `agent/backup-hardening` (also satisfies the carried-over core
backup hardening item).

### T4-08 — Low — A non-throwing UniFFI encoder contains a reachable panic

Affected path: `protocol::encode_digest`.

`encode_digest` is exported as a non-fallible UniFFI function but calls
`assert_eq!` for every recent message ID. An ID with any length other than 16
panics in Rust; generated Swift exposes the function through `try!`, turning a
Rust panic status into an application crash. Current network parsing produces
only 16-byte IDs, so direct remote reachability was not found, but corrupt
persisted data or a future shell call can violate the invariant.

Reproduction: call `encode_digest([], [], [vec![0; 15]])`.

Impact: native app crash from an invariant violation at the UniFFI boundary.

Fix: make the encoder fallible and validate all IDs and counts, then update
both callers; also validate IDs when entering the store.

Planned branch: `agent/t4-fallible-wire-encoders`.

### T4-09 — Medium — Relayd request cardinality and quota admission need caps

Affected paths:

- `ack_envelopes` / `RelayStore::ack_envelopes`
- GET and WebSocket `hints` parsing
- `post_envelope` quota check followed by `insert_envelope`
- WebSocket upgrade defaults

Ack IDs and fetch/WebSocket hints have no endpoint-level count limit. They are
expanded into dynamic SQL and binding vectors while holding the single SQLite
connection mutex. Axum's 2 MiB JSON limit bounds bytes but still permits a very
large ack list. The family quota usage check and insert are separate lock
acquisitions, so parallel posts on the multi-thread runtime can all pass the
same pre-insert usage check and exceed the configured quota. Finally, inbound
WebSocket messages are ignored but use tungstenite's 64 MiB default message
limit.

Reproduction:

- Send an authenticated near-2 MiB ack body containing a very large `ids`
  array; relayd constructs an equally large placeholder SQL statement.
- Submit parallel unique near-quota envelope posts and observe that admission
  is not transactionally coupled to insertion.
- Send a large client-to-server WebSocket text message; it is materialized
  before the application ignores it.

Impact: authenticated denial of service and a bypass of the advertised
per-family disk quota.

Fix: cap ack IDs and fetch/WS hints, dedupe inputs, set a small inbound
WebSocket message/frame limit, and perform prune, quota calculation, and insert
in one SQLite transaction.

Planned branch: `agent/t4-relayd-ingress-limits`.

### T4-10 — Low — Numeric and kind-specific validation is incomplete

Affected paths: message/receipt decoders and SQLite conversions.

- Receipt type is not restricted to delivered/read, and decoded receipt
  `chat_id` is ignored by both shells.
- Wire lamports are `u64` but SQLite writes cast them to `i64`; values above
  `i64::MAX` wrap negative and change ordering/watermark behavior.
- Several non-fallible encoders document that oversized lengths silently
  truncate their `u16`/`u32` prefixes.
- Attachment messages are stored before validating the attachment codec, so
  malformed content becomes durable opaque chat data.

Reproduction: send a validly signed body with `lamport = u64::MAX`, or a
receipt whose type is 255. The frame opens and dispatches; persistence casts
the lamport and receipt storage accepts the extra type.

Impact: bounded state pollution and inconsistent delivery/read behavior from
a malicious accepted contact.

Fix: validate kind-specific bodies before storage, reject lamports that cannot
be represented by the schema, validate receipt type/chat binding, and migrate
public encoders toward fallible checked APIs.

Planned branch: `agent/t4-message-semantics-validation`.

## Controls that held

- `protocol.rs`, `groups.rs`, `content.rs`, and backup cursors check bounds
  before slicing. Their fixed-slice `expect`/`unwrap` calls follow a successful
  exact-length read and are not attacker-reachable panics.
- Pairwise and group opens authenticate AEAD before unpadding, then verify the
  embedded Ed25519 signature before returning plaintext.
- Attachment decode checks its blob length before copying and bounds the blob
  to 360 KiB. Tightening this to the authored 180 KiB policy is sensible, but
  no allocation bomb exists inside this decoder once the enclosing frame is
  capped.
- Android and iOS LAN packet readers reject zero, negative, or greater-than-
  65,535-byte record lengths before allocation/extraction.
- `LanNoiseSession` caps handshake and encrypted records at 65,535 bytes and
  reassembled decrypted frames at 1 MiB. Both shells cap concurrent LAN links
  at eight and close unauthenticated setup after five seconds.
- LAN application frames are delivered only after the Noise remote static key
  maps to an accepted contact. A conflicting later HELLO is rejected by the
  shared router state.
- Relayd validates decoded message ID and hint widths, caps each decoded
  `sealed` value at 512 KiB, uses parameterized SQL values, and has a nominal
  per-family byte quota. T4-09 concerns cardinality and atomic admission, not
  SQL injection or unauthenticated mailbox access.
- The message table dedupes `(chat_id, sender_user_id, lamport)`, and the
  in-memory seen-ID set is bounded to 50,000 entries. T4-01 is specifically
  the carry table's attacker-controlled public-ID dedupe and family exemption.

## Fuzzing recommendation

Add `cargo-fuzz` targets for these no-panic properties:

1. `parse_frame`, `decode_extended_message_body`, and
   `decode_receipt_content` over arbitrary bytes.
2. `decode_group_invite_content` and `decode_group_metadata_update`.
3. `decode_attachment_payload`, `decode_reaction_payload`, and
   `decode_profile_sync_content`.
4. `relay_decode_fetch_page` and `relay_decode_presence_page`, with seed
   corpus entries at each size/cardinality boundary.
5. `BleFrameReassembler::accept` as a stateful sequence target with an
   assertion that retained bytes never exceed the shared frame ceiling.

`cargo-fuzz` and a nightly toolchain are not installed on the Windows
workstation, so targets should live on their own branch and run in Linux CI
with a short smoke duration on every change plus longer scheduled jobs.
