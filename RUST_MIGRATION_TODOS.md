# Rust Migration: Audit Assessment & Sub-Agent TODOs

Assessment of the platform-core audit (baseline commit `68abbe1`) and a
non-overlapping work breakdown for moving duplicated Android/iOS logic into
`core/` (Rust, exposed over UniFFI). Each TODO below lists the files it
**owns** — no two TODOs own the same file, so independent tasks can be handed
to separate agents without merge collisions.

## 1. Audit assessment

The audit was verified claim-by-claim against this checkout. All four
headline findings are **confirmed real**, and the file inventory is accurate
(line counts of `MeshService.kt` = 3,239 and `MeshController.swift` = 1,894
match exactly).

| # | Finding | Verified | Evidence |
|---|---------|----------|----------|
| 1 | BLE framing wire-incompatible | ✅ | Android writes a 4-byte `[index16, total16]` header, max 65,535 fragments (`android/.../mesh/FrameFraming.kt:18`). iOS writes a 2-byte `[index8, total8]` header, max 255 (`ios/CruiseMesh/Mesh/FrameFraming.swift:7-8`). An Android photo frame (>255 fragments) cannot be reassembled by iOS at all — even below 255 fragments the header layouts disagree, so **any** fragmented Android↔iOS BLE frame is corrupt today. |
| 2 | Relay ack drift can destroy carried envelopes | ✅ | Android acks only per `InboundDisposition` (`MeshService.kt:885-950`; `CARRIED` is deliberately left on the relay, `MeshService.kt:896-899`). iOS appends **every** fetched envelope id to `acks` unconditionally (`MeshController.swift:1860-1874`). An iOS proxy-fetch can delete a mailbox envelope before its true recipient consumes it. |
| 3 | Lamport authoring drift | ✅ | Android ratchets past delivered/read receipts via `nextAuthoredLamport` (`android/.../friending/FriendRequestSender.kt:52`). iOS uses bare `highestContiguousLamport + 1` (`ios/CruiseMesh/Friending/FriendRequestSender.swift:21`), so a deleted-and-recreated chat can reuse a Lamport the peer already acked and be silently deduplicated. |
| 4 | Digest-time muling drift | ✅ | Android's digest handler sprays carried foreign envelopes **plus** its own pending messages and pending receipt envelopes (BLE_1TO1_MULING.md Hook B / §6 follow-up paths, `MeshService.kt:1493-1558`). iOS stops after `sprayCarriedEnvelopesTo` (`MeshController.swift:523`). Offline delivery is measurably weaker via iOS mules. |

Secondary claims also check out: backup (`.cmbak`, PBKDF2, AES-GCM) exists
only on Android (`android/.../identity/backup/`, 6 files; iOS has no restore
path); the duplicated pure-policy pairs all exist (`DigestSync`,
`MeshRouterState`, `ReconnectBackoffTracker`, `LanHealthTracker`,
`RelayClient`, `LanEndpointLink`/`LanEndpointSupport`, `AttachmentPayload`,
`MessageInteractions`); and `core/` already provides the right substrate
(`protocol.rs` frame codecs, `store.rs` at 5,213 lines with watermarks and
carried-envelope queries, `crypto.rs`, `groups.rs`, `gossip.rs`,
`lan_session.rs`) with UniFFI scaffolding already wired
(`uniffi::setup_scaffolding!` in `core/src/lib.rs`).

Minor corrections to the audit:

- iOS framing does not "fail" oversized frames with an error — `fragment`
  silently returns `[]` (`FrameFraming.swift:14`), which is worse (silent
  drop, no log).
- The audit's proposed `MeshEngine` event→effect boundary is right, but the
  drift fixes in findings 2–4 are **behavioral decisions**, not just code
  moves. The resolutions are recorded per-TODO below so agents don't have to
  re-litigate them: in each case Android's behavior is the correct one
  (disposition-gated acks, receipt-ratcheted Lamports, full digest spray),
  and 16-bit framing is the canonical wire format.

## 2. Ground rules for every TODO

- Baseline: branch from `main` (or this branch) at/after `68abbe1`.
- New Rust code lives in `core/src/`, exported through `core/src/lib.rs`,
  reachable from both apps via the existing UniFFI setup (see AGENTS.md for
  bindgen steps).
- Port the platform tests alongside the logic: the Kotlin/Swift tests for a
  module become Rust tests; keep thin platform tests only for the FFI seam.
- Native keeps: BLE/CoreBluetooth I/O, sockets/HTTP execution,
  Keychain/Keystore, lifecycle/background scheduling, UI/localization.
- Each TODO owns the listed files exclusively (create, rewrite, or delete);
  it must not edit files owned by another TODO. Shared-file collisions are
  resolved by the dependency ordering in §4.

## 3. TODO list (non-overlapping)

### T1 — Rust BLE framing (`core/src/framing.rs`) — *highest priority, unblocks cross-platform BLE*

Implement fragment/reassemble once in Rust with the canonical **16-bit
index/total** header (Android's format; both fleets update together per the
comment in `FrameFraming.kt:14-16`). Preserve Android's semantics:
`MAX_ATT_VALUE_LEN = 512` cap, `fragment_or_null`-style non-throwing entry
point for GATT-callback callers, ordered-stream reassembler that drops
desynced partials. Replace both native copies with bindings calls; iOS gains
photo-scale frames (>255 fragments) and loses the silent `[]` drop.

**Owns:**
- `core/src/framing.rs` (new), `core/src/lib.rs` export lines
- `android/app/src/main/kotlin/com/cruisemesh/app/mesh/FrameFraming.kt` (delete/thin-wrap)
- `android/app/src/test/kotlin/com/cruisemesh/app/mesh/FrameFramingTest.kt` (port to Rust)
- `ios/CruiseMesh/Mesh/FrameFraming.swift` (delete/thin-wrap)
- `ios/CruiseMeshTests/FrameFramingTests.swift` (port to Rust)

**Decision recorded:** 16-bit header is canonical; the iOS 8-bit format is
retired with no compatibility shim (pre-release fleet updates together).

### T2 — Transactional authoring API (`core/src/authoring.rs`)

Add atomic core operations that read watermarks, assign the Lamport,
construct/encode/seal the `StoredMessage`, generate the msg id, build the
outbound envelope, and persist message + envelope in one store transaction,
returning the encoded frame: `author_pairwise_message`,
`author_group_message`, `author_receipt`, `queue_group_invites`, plus the
friend-request authoring path. Lamport selection must ratchet past
delivered/read receipt watermarks (Android's `nextAuthoredLamport`
semantics) — this fixes the iOS Lamport-reuse bug (finding 3).

**Owns:**
- `core/src/authoring.rs` (new) + `store.rs` transaction additions needed by it
- `android/.../mesh/OutgoingTextEnvelope.kt`, `android/.../chat/MeshSender.kt` (`nextAuthoredLamport` and envelope-builder portions)
- `android/.../friending/FriendRequestSender.kt`
- `ios/CruiseMesh/Mesh/OutgoingEnvelope.swift`
- `ios/CruiseMesh/Friending/FriendRequestSender.swift`
- Their unit tests (port to Rust)

**Decision recorded:** receipt-ratcheting Lamport assignment (Android
behavior) is canonical on both platforms.

### T3 — Content wire codecs: attachments & reactions

Move the attachment payload format (manifest/chunk encode/decode, size and
hash validation) and the reaction **wire codec** into `core/src/protocol.rs`
(or a new `content.rs`). The reaction *reducer* and message-query semantics
are T10, not this task — T3 is bytes-on-the-wire only.

**Owns:**
- `core/src/content.rs` (new) or additions to `protocol.rs`
- `android/.../media/AttachmentPayload.kt`
- `ios/CruiseMesh/Media/AttachmentPayload.swift`
- Codec-only portions of `android/.../chat/MessageInteractions.kt` and `ios/CruiseMesh/Chat/MessageInteractions.swift` (coordinate with T10: T3 extracts the codec functions; T10 owns everything that remains in those two files)
- Their codec tests (port to Rust)

### T4 — LAN-neutral utilities (`core/src/lan_util.rs`)

Manual endpoint parsing, endpoint-link encoding/decoding, subnet candidate
generation, network fingerprinting, endpoint cache expiration, and endpoint
resend policy — all pure functions today duplicated across the two files
below. Sockets, mDNS/NSD discovery, and reachability stay native.

**Owns:**
- `core/src/lan_util.rs` (new)
- `android/.../mesh/LanEndpointLink.kt`
- `ios/CruiseMesh/Mesh/LanEndpointSupport.swift`
- Their tests (port to Rust)

### T5 — Relay wire protocol (`core/src/relay_wire.rs`)

Request/response DTO encoding/decoding, base64url handling, field
validation, and relay URL normalization from the two `RelayClient`
implementations. Native code shrinks to "execute this HTTP request, hand
bytes back." The ack *decision* (which envelope ids to ack) is **not** this
task — it belongs to the T7 engine; T5 only provides the ack-request codec.

**Owns:**
- `core/src/relay_wire.rs` (new)
- `android/.../relay/RelayClient.kt` (codec/validation portions; HTTP execution remains)
- `ios/CruiseMesh/Relay/RelayClient.swift` (same split)
- `ios/CruiseMeshTests/RelayClientTests.swift` + Android equivalents (port codec tests to Rust)

### T6 — Backup & identity serialization (`core/src/backup.rs`)

Port the `.cmbak` container format, PBKDF2 key derivation, AES-GCM
seal/open, passphrase handling, and payload codec to Rust as a core backup
API, and absorb the identity byte-packing duplicated in both identity
stores. Security-sensitive and platform-independent; must land **before**
iOS grows restore support so iOS never gets a second implementation.
Keystore/Keychain wrapping of the passphrase/key stays native, as do the
Android backup screens/service (they call the new API).

**Owns:**
- `core/src/backup.rs` (new)
- `android/.../identity/backup/BackupCodec.kt`, `BackupCrypto.kt`, `BackupFormat.kt`, `BackupPassphrase.kt` (logic moves; `BackupService.kt`/`BackupScreens.kt` become callers)
- Identity serialization/packing portions of both platform identity stores
- Backup tests (port to Rust)

### T7 — MeshEngine: inbound / digest / relay state machine — *largest task; run after T1, T2, T5, T8*

The audit's core recommendation. Build `core/src/engine.rs`: a deterministic
event→effect engine (`link_connected`, `frame_received`, `timer_fired`,
`relay_page_fetched`, … → `send_frame(route, bytes)`,
`relay_request(req)`, `ack_envelopes(ids)`, `notify_changed(...)`,
`close_link`, …) that subsumes the duplicated logic in `MeshService.kt` and
`MeshController.swift`:

- HELLO and DIGEST processing (including the wire-chatId-vs-HELLO check)
- inbound envelope classification → `InboundDisposition` (SEEN / EXPIRED /
  CONSUMED / CARRIED) shared by BLE, LAN, and relay paths
- receipt generation and synchronization
- gossip dedup and forwarding (uses existing `gossip::SeenIds`)
- foreign-envelope carrying and spray selection
- group recognition and delivery
- relay upload, poll paging, and **disposition-gated acknowledgement**
- friend/profile/directory message handling
- LAN endpoint hints and health-probe scheduling decisions

**Decisions recorded:** relay acks are gated on disposition — `CARRIED`
never acks (fixes finding 2, Android behavior). Digest-time spray includes
carried foreign envelopes **and** own pending messages **and** pending
receipt envelopes (fixes finding 4, Android behavior).

**Owns:**
- `core/src/engine.rs` (new) + supporting store queries
- `android/.../mesh/MeshService.kt` (shrinks to lifecycle + BLE I/O + effect executor)
- `ios/CruiseMesh/Mesh/MeshController.swift` (same)
- Engine-behavior tests currently in both platforms' mesh test suites

This TODO is internally phased (a: inbound classification + relay ack; b:
digest reconciliation + spray; c: HELLO/friend/profile/LAN-hint handlers)
but must be one agent's task — the phases all edit the same two files.

### T8 — Pure transport state & policy (`core/src/transport_policy.rs`)

Port the four already-pure state machines and the send-plan selection
policy: `DigestSync`, `MeshRouterState`, `ReconnectBackoffTracker`,
`LanHealthTracker`, and route selection / retry / close policy. Native code
executes sends; Rust decides which routes to use and when to retry or
close. T7 consumes these; land T8 first.

**Owns:**
- `core/src/transport_policy.rs` (new)
- `android/.../mesh/DigestSync.kt`, `MeshRouterState.kt`, `ReconnectBackoffTracker.kt`, `LanHealthTracker.kt`
- `ios/CruiseMesh/Mesh/DigestSync.swift`, `MeshRouterState.swift`, `ReconnectBackoffTracker.swift`, `LanHealthTracker.swift`
- `ios/CruiseMeshTests/DigestSyncTests.swift`, `MeshRouterStateTests.swift`, `ReconnectBackoffTrackerTests.swift` + Android equivalents (port to Rust)

### T9 — Friend import reconciliation & friend-text parsing

Absorb the duplicated native `extractFriendToken` functions into the
existing `identity::parse_friend_text`, and move the import-reconciliation
rule from `RelayImport.kt` (preserve existing relay details on re-import)
into core so iOS stops blind-upserting imported contacts.

**Decision recorded:** Android's preserve-existing-relay-details rule is
canonical.

**Owns:**
- Additions to `core/src/identity.rs` / `core/src/store.rs` (import upsert)
- `android/.../relay/RelayImport.kt`
- iOS friend-import call sites (the upsert helpers, not `MeshController.swift` — its call-site line changes to the new API go through T7's owner if concurrent)
- Their tests (port to Rust)

### T10 — Semantic message queries (`core/src/store.rs` extensions)

Visibility classification, unread counts, reaction reduction, gap
detection, receipt/tick status, and reply-target resolution as store-level
Rust queries. Localized preview strings and visual formatting stay native.
Takes over whatever remains of `MessageInteractions.kt`/`.swift` after T3
extracts the wire codecs.

**Owns:**
- Query additions to `core/src/store.rs`
- `android/.../chat/MessageInteractions.kt` and `ios/CruiseMesh/Chat/MessageInteractions.swift` (post-T3 remainder: reducer + query logic)
- The platform view-model query helpers these replace, and their tests (port to Rust)

## 4. Dependency order / parallelism

```
Wave 1 (fully parallel): T1  T2  T3  T4  T5  T6  T8  T9
Wave 2 (after T1, T2, T5, T8 land): T7
Wave 3 (after T3 lands; parallel with T7): T10
```

- T1, T4, T6 are entirely self-contained.
- T2 and T5 touch `store.rs`/`protocol.rs` additively — additive Rust
  changes merge cleanly; only file *ownership* (native deletions) is
  exclusive.
- T7 is the only task allowed to edit `MeshService.kt` /
  `MeshController.swift`. Wave-1 tasks that today have call sites inside
  those files (T1 framing calls, T2 authoring calls, T9 import calls) land
  their core API + standalone native file changes first; T7's owner swaps
  the call sites during the engine rewrite.
- Migration order matches the audit: framing → authoring → engine →
  codecs/policies/utilities → backup (T6 may land any time in wave 1 but
  must precede any iOS restore feature).

## 5. Behavioral decisions summary (do not re-litigate per task)

1. **Framing:** 16-bit `[index16, total16]` header, 512-byte ATT cap,
   non-throwing fragment entry point. No 8-bit compatibility mode.
2. **Relay acks:** disposition-gated; `CARRIED` envelopes are never acked by
   a proxy fetcher.
3. **Lamport assignment:** ratchets past delivered/read receipt watermarks,
   atomically inside the authoring transaction.
4. **Digest muling:** spray = carried foreign envelopes + own pending
   messages + pending receipt envelopes, under the existing per-exchange
   byte budgets.
5. **Friend import:** re-import preserves existing relay details.

## 6. Implementation status

Implemented in this branch:

- **T1-T6 and T8-T10:** complete. Their protocol framing, transactional
  authoring, codecs, LAN utilities, relay wire format, backup crypto,
  transport policy, friend import/text parsing, and semantic queries now
  live in `cruisemesh-core`. Android and iOS retain thin adapters plus
  platform storage, UI, and transport execution.
- **T7a/T7b:** complete. Rust now makes inbound disposition/relay-ack
  decisions and builds the complete digest spray plan (carried envelopes,
  locally authored messages, and receipt envelopes). HELLO identity checks
  also use the same core rule on both platforms.
- **T7c:** deliberately partial. Opened-content dispatch, OS lifecycle,
  BLE/CoreBluetooth, sockets/HTTP, notifications, and other side effects
  remain in `MeshService`/`MeshController`. Moving the remaining content
  dispatch into a full event-to-effect interpreter is a separate structural
  refactor; it is no longer needed to eliminate the duplicated protocol and
  policy implementations identified by this audit.

Validation at implementation time: all Rust core tests and mesh simulations,
Android JVM unit tests, and the iOS simulator test suite pass.
