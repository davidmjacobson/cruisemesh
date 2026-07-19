# TODO — consolidated backlog

Updated 2026-07-19. This is the single live TODO file. It replaces
`DTN_TODOS.md`, `UI_TODOS.md`, `LAN_TODOS.md`, `RUST_MIGRATION_TODOS.md`,
`DTN_AUDIT.md`, and `UI_UX_REVIEW.md` — everything actionable in those files
shipped (DTN D1–D7, UI U1–U13, LAN L1/L2/L4; see git history and the removed
files' completion records) except the items carried over in §2 below.

§1 is new work from David's field observations. §3 has the ground rules;
read them before writing code.

---

## 0. Shipped to master (2026-07-19)

Done and merged; kept in §1/§2 below as reference (bodies describe what was
built). Do not re-open.

- **T4 — adversarial payload review** ✅ all 10 findings T4-01…T4-10 + review
  doc merged (PRs #76–86). `specs/adversarial-payload-review.md`. No
  memory-safety/forgery bug; the DoS hardening (carry-queue budget, BLE
  reassembly cap, relay/relayd admission limits, sender authorization, KDF
  range, digest fail-safe, message-semantics validation) is live. Verified:
  Rust workspace 330, Android 235, iOS per-branch on the Mac.
- **T4 fuzzing** ✅ decoder fuzz targets + CI smoke merged (PR #89).
- **T9 — isolation verdict shown too soon** ✅ merged (PR #87): verdict gated
  on sweep completion, neutral in-progress state, resets on network change.
- **Release diagnostic logging** (Android; §1.5) ✅ merged (PR #88).
- **`backup_to` hardening** (was a §2 carryover) ✅ folded into T4-07.

Still open and worth doing next (my read): **T1 swipe-to-reply** and **T2
mule count** are the highest-value self-contained 🔴 code items. **T11
friending reciprocity** is 🔴 but blocked on a device log (David repro +
logcat). **T12 QR density** is a quick 🟡 (root cause already diagnosed).
**T13 iOS diagnostic-log parity** now that the Android half shipped. Larger:
**D9** group receipts. Field/device: **L3/V1/V2**.

---

## 1. New items (field observations)

### T1 🔴 Swipe-to-reply, Signal-style (both platforms)

Swipe right on a message bubble: the bubble follows the finger to the right,
then snaps back, and a reply to that message starts. **The keyboard must open
when the reply starts** (focus the composer).

- Reply plumbing already exists via the long-press overlay
  (`MESSAGE_LONGPRESS_OVERLAY.md`) — reuse the same reply-start path; this item
  adds the gesture + animation + composer focus only.
- Android: `chat/ChatScreen.kt` + `chat/GroupChatScreen.kt` — horizontal drag
  on the bubble row (`pointerInput` drag with an animated offset and
  resistance/clamp), snap-back spring, then set the reply target and request
  composer focus (`FocusRequester` + keyboard).
- iOS: `UI/ChatView.swift` / `UI/GroupChatView.swift` — same gesture
  (`DragGesture` with offset animation; don't fight the scroll gesture —
  require mostly-horizontal drags).
- Accept: swipe past threshold starts a reply and opens the keyboard; below
  threshold it just snaps back; scroll is not hijacked; works in 1:1 and group.

### T2 🔴 Show how many messages you're muling

No way to see how many other people's messages this phone is carrying.

- Count = carried envelopes in the core store (the mule queue). Add a
  read-only core accessor if one doesn't exist; don't expose contents, just
  counts (carried total; optionally bytes).
- Surface: mesh status pill legend/sheet (both platforms) — e.g.
  "Carrying 12 messages for others" — and/or the Advanced diagnostics screen.
  Zero state: hide the row or say "Not carrying any messages right now".
- Accept: count visible, updates live (or on legend open), matches the store.

### T3 🟡 Evaluate Google Nearby Connections (research spike, no product code)

Nearby Connections may be the strongest practical foundation for
Android↔iPhone offline communication: it now has documented Android and Swift
implementations and abstracts Bluetooth, BLE, and Wi-Fi into one encrypted
peer-to-peer API.

- Deliverable: a short spec in `specs/nearby-connections-eval.md` — verify the
  cross-platform (Swift) support claim and its maturity, permissions story,
  background behavior, throughput vs our BLE GATT + LAN stack, whether it
  could coexist with (or replace) our BLE advertising without breaking the
  BLE/audio coexistence constraints, and a migration/fallback story
  (older clients must keep working).
- No implementation until the spec is reviewed. Findings only.

### T4 🔴 Adversarial payload review (security)

Look at CruiseMesh from an attacker's perspective: can a malformed payload
have malicious effect? This is an authorized review of our own codebase.

- Attack surface to review (at minimum): `core/src/protocol.rs` decode paths,
  `relay_wire.rs`, fragment reassembly (the 512-byte GATT frame path),
  attachment decode, LAN packet framing (`LanTransport.readPacket` length
  handling), the Noise handshake pre-auth surface, relayd request parsing,
  and the UniFFI boundary (panics in core = native crash in the app).
- Classes to check: panics/unwraps reachable from untrusted bytes (crash
  DoS), length-field allocation bombs, protocol confusion between kinds,
  duplicate/replay handling, oversized-envelope handling on every ingest
  path (relayd now caps at 512 KiB — do the P2P paths?), and anything that
  writes to the store before authentication.
- Deliverable: findings written up (severity + repro + fix), fixes as
  separate small PRs. Consider `cargo-fuzz` targets for the decoders as a
  lasting artifact.

**Status (Codex, 2026-07-19): review DONE, 9 of 10 fixes on branches.**
Findings doc: `specs/adversarial-payload-review.md` (on
`agent/t4-adversarial-payload-review`) — 10 findings T4-01…T4-10, no
memory-safety/forgery issue; the risk is DoS (unbounded carry storage,
33 MiB BLE reassembly, unbounded relay/relayd admission, unauthorized-sender
persistence, KDF CPU bomb). One hardening branch per finding, each +1 commit
off master, **not yet merged or independently re-verified by me:**

| Finding | Branch | State |
|---|---|---|
| T4-01 carry storage exhaustion | `agent/t4-carry-queue-hardening` | committed |
| T4-02 BLE 33 MiB reassembly | `agent/t4-p2p-frame-limits` | committed |
| T4-03 unbounded relay response | `agent/t4-relay-response-limits` | committed |
| T4-04 unknown-sender persistence | `agent/t4-inbound-sender-authorization` | committed |
| T4-05 group/identity amplification | `agent/t4-structured-content-limits` | committed |
| T4-06 failed-store must not ack | `agent/t4-durable-ingest-result` | **DONE (`02ec63d`)** — core `CoreInboundDisposition::Failed`; iOS handlers propagate the primary store throw + split delivery catch from open catch; Android wraps direct delivery. Core 255+8, Android 233, all green; iOS pending Mac |
| T4-07 backup KDF CPU bomb | `agent/t4-backup-hardening` | committed (also the carried-over backup_to hardening) |
| T4-08 `encode_digest` panic | `agent/t4-digest-encoding` | committed |
| T4-09 relayd ingress cardinality/quota | `agent/t4-relayd-request-limits` | committed |
| T4-10 numeric/kind validation | `agent/t4-message-semantics-validation` | committed |
| fuzzing | `agent/t4-decoder-fuzz-targets` | committed (`f660445`) — was interrupted uncommitted; finished + CI smoke added |

All 10 findings now have a branch (T4-06 implemented by me; the rest by
Codex).

**Verification (2026-07-19, independent re-run):** all 9 Codex branches pass
the Rust workspace suite (`cargo test --workspace` = core + relayd), 255–305
tests each, 0 real failures. One WebSocket integration test
(`ws_live_push_after_connect`) flaked once under parallel-build load; it
passes 4/4 in isolation and on a clean full re-run — environmental, not a
regression. All 10 branches merge cleanly onto current `master`
individually. Fix logic read and confirmed sound for the High-severity
findings T4-01 (BLAKE2b content-digest dedupe + 64 MiB carry budget +
expiry/hop validation) and T4-02 (per-fragment + cumulative BLE reassembly
cap), and the most complex shell change T4-04 (core `core_pairwise_sender_authorized`
gate, correctly called before persistence in the shells). Android spot-check
on T4-04 (`agent/t4-inbound-sender-authorization`): 233 unit tests, 0
failures. iOS suites for every branch still await Mac validation.

Remaining work: (a) Merge sequencing — the branches share hot files
(`engine.rs`: T4-01/04/06; `protocol.rs`: T4-02/05/08/10; `store.rs`:
T4-01/05/07/10; `lib.rs`: 5 branches; generated bindings: 8 branches). Merge
one at a time onto master, and DO NOT merge the generated-binding diffs —
regenerate them with uniffi-bindgen after each source merge, then re-run
suites. `agent/t4-relayd-request-limits` (T4-09) is fully independent (relayd
only) and can land anytime. (b) iOS validation on the Mac once reachable.

### T5 🟡 Onboarding copy rework

Current copy is awkward ("mesh off without this" style phrasing). Rebrand the
script around one clear idea: **CruiseMesh uses virtually every form of
connectivity available to your phone to help your messages get through.**

- Rewrite `ONBOARDING_SCRIPT.md` first (it's the source of truth for the four
  slides), get sign-off, then apply to `ui/OnboardingScreen.kt` +
  `UI/OnboardingView.swift` via the string resources (post-U11, copy lives in
  `strings.xml` / `Localizable.xcstrings` — no hardcoded literals).
- Permission asks should say what the permission buys in plain words, not
  what breaks without it.

### T6 🟡 Show how a delivery confirmation returned

Return-receipt visibility: when a message is confirmed delivered, say how the
confirmation came back — e.g. "Delivered — confirmed via Bluetooth".

- Receipts already flow (✓✓); Message info already shows the *incoming*
  message's route. Record the transport the receipt arrived on and surface it
  in Message info (structured sheet from U10). Tick glyphs don't change.
- Core-first: the receipt ingest path is shared; store the receipt's arrival
  transport alongside the watermark if not already present.

### T7 🟡 Adaptive connectivity guidance (keep Wi-Fi on for LAN delivery)

Android's adaptive connectivity ("switch to mobile data when Wi-Fi has no
internet") can drop the ship's internet-less Wi-Fi — but we need that Wi-Fi
association for LAN delivery. We can't toggle the setting programmatically;
this is a guidance/detection item.

- Add plain-language guidance where it helps: onboarding (Wi-Fi slide) and
  the LAN diagnostics area — "Keep Wi-Fi connected even when it has no
  internet; CruiseMesh uses it to reach phones near you."
- Nice-to-have: detect the symptom (repeated Wi-Fi loss shortly after join
  while cellular stays up) and show the tip contextually rather than always.
- Note: relay sync deliberately rides cellular when Wi-Fi is dead — that
  stays; only the *association* to Wi-Fi needs preserving.

### T8 🟢 Pin name + photo while scrolling

The chat header (contact/group name + avatar) should stay frozen at the top
while the message list scrolls, on both platforms. Verify the same for any
other screen where the identity header currently scrolls away (contact
details, profile).

### T9 🔴 Isolation verdict shown too soon

Advanced settings can show "This Wi-Fi appears to block phone-to-phone
traffic…" (`LanTransport.kt` `ISOLATION_DIAGNOSTIC`) when the sweep may not
even have completed yet.

- The verdict must only appear after a *completed* sweep of ≥253 candidates
  (the `SweepOutcomes` isolation rule). While a sweep is running or none has
  completed on this network, show a neutral in-progress state ("Checking this
  network…") or nothing.
- Clear the verdict on network change (`onNetworkJoined`) and on peer
  evidence — a stale verdict from the last network must never show.
- Add a unit test for the display-state transitions (pure helper per the
  invariant-4 pattern in §3).

### T10 🟢 Safety words: hide deeper or explain

Safety words (contact verification words) are an obscure concept surfaced too
prominently.

- Either move them behind an explicit "Verify contact" affordance in contact
  details, or keep placement but add a one-line plain explanation ("Match
  these words with your friend's screen to confirm it's really them") —
  propose one in the PR, don't do both halfway.
- Touchpoints: `UI/ContactDetailsSheet.swift`, `UI/FriendsView.swift`,
  `UI/ProfileView.swift`, Android friend card / contact details +
  `strings.xml`.

### T11 🔴 iPhone→Android friending doesn't reciprocate

Field report: iPhone scans an Android QR and adds the contact, but the
Android never auto-adds the iPhone back.

- The code paths look symmetric and correct: iOS sends the mutual kind=3
  (`FriendsView.confirm()` → `FriendRequestSender.sendMutualFriendRequest`),
  and Android's `MeshService.handleIncomingFriendRequest` imports the sender
  even when it isn't a contact yet — the import runs *before* the dedupe
  check, so even a re-sent request should import. So the failure is delivery
  or open/verify, not the handler.
- Diagnose first (David + orchestrator — logs, not code): repro while
  capturing Android logcat filtered on
  `FriendRequest|Dropping envelope|Dropping friend request`. Outcomes:
  - Nothing logged → the kind=3 never arrived. Check iOS Console for
    "Friend request queued for later delivery" (means `sendToUserId` found no
    authenticated link at scan time and it went to mules/relay).
  - "failed to decode body" / "failed to parse FriendCard" → cross-platform
    encoding bug; "payload identity doesn't match verified sender" →
    card/key mismatch.
- Code suspects once diagnosed: (a) no iOS↔Android authenticated link at scan
  time and the relay fallback needs the *peer's* relay token — verify iOS
  posts to the contact's mailbox, not only its own family; (b) if the direct
  send reports success but the write is lost, confirm hidden kind=3 messages
  are covered by digest resend on reconnect — if hidden kinds are excluded
  from digests, a lost first send is never retried.

### T12 🟡 iPhone QR is far too dense

Two platform-specific causes plus one shared root cause, all verifiable in
code today:

- **iOS error correction is higher:** `QRCodeGenerator` (FriendsView.swift)
  sets `inputCorrectionLevel = "M"`; Android's zxing default is L. That alone
  is ~25% more codewords → 1–2 QR versions denser. Set iOS to "L"
  (screen-to-screen scanning doesn't need damage tolerance).
- **iOS renders smaller:** 240 pt vs Android's 512 px bitmap — the same
  module count looks denser and scans worse. Render at least as large
  relative to screen width as Android does.
- **Shared root cause — the payload is bloated:** `FriendCard` serializes
  `sign_pk`/`agree_pk` as JSON *number arrays* (~4 chars per key byte), then
  the whole JSON is base64url'd into `CMFRIEND1:` inside
  `https://cruisemesh.app/f#…`. With relay URL + token the string easily
  exceeds 500 chars. Fix: a compact `CMFRIEND2:` form (binary layout or
  base64-fields JSON, in core `identity.rs` next to `make_friend_link`) —
  roughly halves the payload; `parse_friend_text` keeps accepting
  `CMFRIEND1:` for old cards. Decide separately whether the relay token (the
  longest field) belongs in the QR at all or only in the share-text form.
- Accept: iPhone and Android QRs for the same card have comparable module
  counts; cross-platform scan round-trip still works for both old and new
  link forms.

### T13 🟡 iOS diagnostic-log capture + share (Android parity)

Android release builds now capture a metadata-only diagnostic log and share
it (see §1.5 Recently shipped). iOS has no equivalent — it logs via `os.log`
with no in-app export, so a field iPhone can't hand over a log.

- Add an `OSLogStore`-based export: read this process's entries since the last
  launch (or a bounded window), write them to a temp file, present a share
  sheet. Same content rule as Android — the app already logs metadata only
  (kinds, counts, user-id hex, addresses), never message content; verify no
  Swift log site interpolates message text before shipping.
- Surface it in the iOS Advanced settings (the `ProfileView` Advanced
  destination from U1) with the same opt-in switch semantics if capture needs
  to be gated; `OSLogStore` reading is on-demand so an always-available
  "Share diagnostics" button may be enough — state which in the PR.
- Validate on the Mac host (currently unreachable — check first).

---

## 1.5 Shipped — see §0

Everything formerly staged here (release diagnostic logging, the T4 fixes,
T9, fuzzing) has merged to master. See §0 for the list with PR numbers.

---

## 2. Carried over (still open from the retired TODO files)

- **D8 — Periodic re-digest on long-lived links.** A frame lost mid-session
  waits for the next reconnect. Add a jittered 3–5 min re-digest interval
  decision to `core/src/transport_policy.rs` (+ tests), timer wiring in both
  shells. Digest exchange is already idempotent.
- **D9 — Per-group digests + wire group receipts (largest).** Replace
  `resendGroupOutboundToPeer`'s lamport-0 full resend with per-group digest
  frames; put per-member group delivered/read receipts on the wire,
  aggregated to "✓✓ = all members". Protocol-compatible: old clients ignore
  unknown digest chat_ids. Needs a mini-spec in the PR; core-first.
- **L3 — LAN field validation (David + orchestrator, NOT for agents).**
  On-device checklist: "/24 sweep ~5s after LAN session ready", full sweep
  only after empty /24, tie-break logs, hint replay after HELLO, sweep
  summary line + denied counts under the WireGuard VPN.
- **L5 — UDP multicast beacon (optional).** Third discovery protocol for
  networks that filter mDNS but pass multicast. Plaintext beacon = token +
  port only, no identity; Noise still gates connections. Gate behind the
  loneliness check; treat replies as `onPeerEvidence`. Skip if L3 field data
  shows isolation dominates.
- **V1 — LAN validation gates** from `specs/same-lan-transport.md` on real
  networks (human + devices).
- **V2 — Field metrics for the cruise test.** Per-message delivery latency +
  mode mix (BLE / LAN / mule / relay / push) logged locally + an export
  screen, so the field test produces data. Small, both platforms.
- **Relay presence "Phase 2"** (from `CONNECTIVITY_INDICATOR.md`): deferred —
  touches the live relay server; needs an explicit go-ahead.
- ~~Core `backup_to` hardening~~ — ✅ done, folded into T4-07 (see §0).

---

## 3. Ground rules (unchanged; condensed from the retired files)

1. **Worktree per task** (`git worktree add ../CruiseMesh-<slug>`), never the
   shared main checkout. Fresh worktrees need `kotlin-gen/` + `jniLibs/`
   copied from the main checkout and a `cargo build` in the worktree for JVM
   unit tests.
2. **Commit author email** must be
   `14227840+davidmjacobson@users.noreply.github.com` or the push is rejected.
3. **Base on current `master`.** One branch per item (`agent/<slug>`); don't
   open PRs unless asked; paste fresh test output.
4. **Decisions live in the Rust core** (`core/src/`), exported via UniFFI;
   both platforms call it. Never fix shared behavior in one platform's shell.
   Pure schedule/policy logic = plain classes, `@Synchronized` leaf monitors,
   no Android imports, unit-tested directly.
5. **Endpoint privacy invariant:** each phone advertises ONLY its own
   endpoint, sealed pairwise per contact. Discovered or third-party IPs are
   never forwarded to anyone.
6. **DTN ack safety:** never ack a relay copy unless this device was the
   envelope's sole true endpoint consumer; a carried 1:1 envelope is removed
   only on digest-proof of receipt, never on dispatch. When in doubt, don't
   ack.
7. **Copy rules:** sentence case; status/error copy strictly literal; no
   protocol jargon in user-facing text. Strings go in resources
   (`strings.xml` / `Localizable.xcstrings`) — CI rejects hardcoded literals.
8. **Product bar:** obvious for family members on the surface; capability for
   power users behind Advanced. When in tension, simplicity wins the surface.
9. **Tests:** Android `cd android && ./gradlew :app:testDebugUnitTest
   --rerun-tasks`; core `cargo test -p cruisemesh-core`; iOS builds/tests on
   the Mac validation host (currently unreachable — MacinCloud instance
   stopped; check before starting iOS work).
