# AGENT-TODO — the recommended plan

Written 2026-07-19 by the orchestrator. `TODO.md` stays the record of the raw
backlog; this file is the plan of record — what we're actually doing, in what
order, and how. Where an item was reshaped from its TODO.md form, the plan
below is the recommended version (T7 is merged into T15; T17 is the
connection-request inbox, not open delivery; L5 is dropped). When an item
ships, tick it in `TODO.md` §0 and delete its section here.

Ground rules in `TODO.md` §3 apply to every item: worktree per task,
core-first, commit author email, strings in resources, test commands.

---

## Wave plan

| Wave | Items | Why |
|---|---|---|
| 1 — quick wins | T14, T12, T16 | One string; a diagnosed bug; a small clean feature. Independent, parallelizable. |
| 2 — flagship UX | T1, T8, T6 | Swipe-to-reply is the top 🔴; T8/T6 share chat-screen files with T1 — sequence them, don't parallelize. |
| 3 — cruise readiness | V2, T15, T13 | Make the next cruise test produce data and survive ship Wi-Fi. T6 must land before V2. |
| 4 — copy + polish | T5, T10 | Serialized on David's wording sign-off. |
| 5 — DTN core | D8, then D9 | D9 is the largest item; D8 is a warm-up in the same subsystem. |
| Spikes (anytime) | T3, T17 | Spec-only; can run alongside any wave. |
| Blocked on David | T11, L3+V1 session | Device sessions; prep work noted below. |

---

## Wave 1 — quick wins

### T14 🟢 Scan-button label (XS)
`ui_search_this_24_network` → "Search my LAN" in `strings.xml:129`. iOS has no
equivalent mislabel (verified — no user-facing "/24" string). While there,
check `LanTransport`'s clamp behavior against the explainer string
`ui_subnet_search_probes_only_tcp_45892_with_low` ("never expands to a /16")
and fix it if stale. Fold into any wave-1 PR.

### T12 🟡 iPhone QR too dense (M)
Root cause already diagnosed. Three commits, one branch:
1. iOS quick fixes: `inputCorrectionLevel` M→L in `QRCodeGenerator`
   (FriendsView.swift); render at least the Android-equivalent fraction of
   screen width. Ships value even if (2) stalls.
2. Core: compact `CMFRIEND2:` in `identity.rs` next to `make_friend_link` —
   binary layout (version byte ‖ sign_pk ‖ agree_pk ‖ len-prefixed name ‖
   optional relay URL ‖ optional token), base64url'd. Roughly halves the
   payload vs CMFRIEND1's JSON number arrays. `parse_friend_text` accepts
   CMFRIEND1 forever (old cards in the field).
3. Emit CMFRIEND2 on both platforms; round-trip tests old↔new, iOS↔Android.

The relay token stays in the QR — it's how a new friend reaches you via relay
before any direct link exists, and the binary form shrinks it enough.
Acceptance: comparable module counts on both platforms for the same card;
cross-platform scan round-trip works for both link forms.

### T16 🟡 Rename contacts (S–M)
1. Core: nullable `nickname TEXT` on contacts (the `ensure_column` migration
   pattern, store.rs:309 style), `set_contact_nickname(user_id,
   Option<String>)`, resolve helper `display_name = nickname ?? name`;
   `Contact` (store.rs:171) gains the field. Empty string clears. Regenerate
   bindings.
2. UI both platforms: "Nickname" edit in Contact details; resolved name flows
   to chat list, chat header, group member rows, receipt lines. Card name
   stays visible as "also known as" and remains the verification identity.

Invariant, with a test asserting it: nickname is presentation-only — never
serialized into FriendCards, digests, or any wire format.

---

## Wave 2 — flagship UX

### T1 🔴 Swipe-to-reply, Signal-style (M)
1. Android (`chat/ChatScreen.kt`, `chat/GroupChatScreen.kt`): `pointerInput`
   horizontal drag on the bubble row; `Animatable` offset with resistance
   (e.g. half-rate past 40 dp, clamp ~80 dp); release past threshold →
   snap-back spring + set the existing long-press-overlay reply target +
   `FocusRequester.requestFocus()` on the composer (opens the keyboard).
   Below threshold → snap back only.
2. iOS (`UI/ChatView.swift`, `UI/GroupChatView.swift`): `DragGesture` gated on
   |dx| > |dy| so scrolling stays intact; same threshold/snap-back; reply
   start through the same path the context menu uses; `@FocusState` focuses
   the composer.
3. Reuse the reply plumbing from `MESSAGE_LONGPRESS_OVERLAY.md` — no new
   state model, no core changes.

Tests: threshold/clamp math as a pure helper with Android unit tests; iOS
manual. Accept: past-threshold swipe starts a reply with keyboard open;
below-threshold snaps back; scroll not hijacked; works in 1:1 and group.

### T8 🟢 Pin name + photo while scrolling (S)
Android: keep the chat header outside the `LazyColumn` (pinned top bar). iOS:
pinned header outside the `ScrollView` / `.safeAreaInset(edge: .top)`. Audit
contact details + profile for the same. Pure layout. Do in the same wave as
T1 to avoid conflicting edits to the chat screens.

### T6 🟡 Show how a delivery confirmation returned (S–M)
Core-first. `record_receipt` (store.rs:1294) takes no transport today;
messages already have `arrival_transport` with an `ensure_column` migration —
copy that pattern:
1. Core: nullable `via_transport INTEGER` on `receipts`; new param on
   `record_receipt`; update transport only when the watermark advances —
   re-sent receipts for the same watermark must not overwrite the transport
   that first confirmed it. Expose in the query Message info reads.
   Regenerate bindings.
2. Shells: every `record_receipt` call site passes the transport of the link
   the receipt arrived on (BLE / LAN / relay / carried). Message info gains
   one line: "Delivered — confirmed via Bluetooth". Tick glyphs unchanged.

Lands before V2 and D9 — both build on this column.

---

## Wave 3 — cruise readiness

### V2 — Field metrics for the cruise test (S–M)
Highest-leverage small item: without it the cruise test produces anecdotes,
not data. Core-first: a local `delivery_metrics` table (message-id hash,
sent_at, delivered_at, receipt `via_transport` from T6, arrival transport,
hop count); an export accessor (CSV/JSON string); both platforms add "Export
field metrics" next to the diagnostic-log share. Metadata only, same rule as
the diagnostic log. Sequence after T6.

### T15 🔴 Keep the user on Wi-Fi (M) — absorbs T7
One item, three phases, one branch:
1. **Guidance** (was T7): Wi-Fi onboarding slide + LAN diagnostics copy —
   "Keep Wi-Fi connected even when it has no internet; CruiseMesh uses it to
   reach phones near you." Coordinate wording with T5.
2. **Hold the association (the real fix):** Android `requestNetwork` with
   `TRANSPORT_WIFI` and *without* `NET_CAPABILITY_INTERNET`/`VALIDATED`, held
   by MeshService while the mesh is up — the documented mechanism that stops
   adaptive connectivity from tearing down an internet-less Wi-Fi
   association. Must coexist with the relay-sync cellular request
   (`MeshService.relayBindTarget()` — independent requests, separate
   callbacks), be a no-op under the WireGuard VPN test env, and release when
   the mesh stops (battery).
3. **Detect + prompt:** track "Wi-Fi lost within N min of join while cellular
   stayed up" and repeated captive-portal re-auth; after 2+ occurrences show
   the tip contextually. The ~1 h ship-Wi-Fi timeout is a captive-portal
   re-auth we cannot automate — detect it and say "Ship Wi-Fi signed you out —
   rejoin to keep nearby delivery fast." Don't fight the OS.

iOS: guidance strings only (no adaptive-connectivity equivalent).
Field-verify phase 2 on the Pixel during the L3 session.

### T13 🟡 iOS diagnostic-log capture + share (S–M, needs Mac)
`OSLogStore(scope: .currentProcessIdentifier)`, entries since last launch
(bounded window), temp file, share sheet from the Advanced settings
destination. No opt-in switch — reads are on-demand, so an always-available
"Share diagnostics" button (simpler than Android because capture isn't a
running cost); state that in the PR. Pre-ship audit: grep Swift log sites for
interpolated message text — metadata only. Check Mac reachability first.

---

## Wave 4 — copy + polish

### T5 🟡 Onboarding copy rework (S code, serialized on review)
Rewrite `ONBOARDING_SCRIPT.md` around one idea: **CruiseMesh uses virtually
every form of connectivity your phone has to get your messages through.**
Permission slides say what the permission buys ("Nearby access lets phones
hand messages to each other directly"), not what breaks without it. **Stop
for David's sign-off on the script before touching code.** Then apply via
`strings.xml` / `Localizable.xcstrings` only. One coordinated pass over
onboarding strings together with T15's guidance copy.

### T10 🟢 Safety words (S)
Move them behind an explicit **"Verify contact"** row in contact details
(both platforms), which opens the words plus a one-line explanation ("Match
these words with your friend's screen to confirm it's really them"). Removes
them from the friend-card/first-run surface entirely — simplicity wins the
surface. Touchpoints: `UI/ContactDetailsSheet.swift`, `UI/FriendsView.swift`,
`UI/ProfileView.swift`, Android friend card / contact details + `strings.xml`.

---

## Wave 5 — DTN core

### D8 — Periodic re-digest on long-lived links (S)
Decision function in `core/src/transport_policy.rs`
(`should_redigest(now, last_digest_at, jitter_seed) -> bool`, 3–5 min
jittered) + unit tests; both shells call it from their existing link
maintenance tick and re-run the digest exchange (already idempotent). No wire
change. Do immediately before D9 — same subsystem.

### D9 — Per-group digests + wire group receipts (L)
The largest item; plan for it to be the only thing in flight while core
lands. Mini-spec in the PR first, then:
1. Core: per-group digest frames keyed by group chat_id, replacing
   `resendGroupOutboundToPeer`'s lamport-0 full resend; per-member group
   delivered/read watermarks in the store; aggregation rule "✓✓ = all
   members".
2. Wire: new digest chat_id scope — old clients ignore unknown chat_ids. The
   spec must prove compatibility against the T4-10 validation rules so the
   hardened decoders don't reject the new frames.
3. Shells: send/receive wiring + group Message info showing per-member state.

Order: spec → core+tests → shells → cross-platform group field test.
Sequence after T6 (builds on the receipt `via_transport` column).

---

## Spikes (spec-only, no product code)

### T3 🟡 Nearby Connections evaluation (S)
Timebox to one session; deliverable `specs/nearby-connections-eval.md`
answering the five questions in TODO.md. Going-in prior: the Swift
implementation is real but young, iOS **background** operation is the likely
killer (our BLE GATT mesh delivers while backgrounded; Nearby is
foreground-oriented), and replacing our BLE advertising would re-open the
A2DP coexistence problem we already tuned around. Expected outcome: a
documented "no, because…" we can cite, plus any protocol ideas worth
stealing (e.g. their Wi-Fi upgrade path).

### T17 🟡 Connection-request inbox (S) — the onboarding spike
Not open delivery — a stranger on the same relay can send a **connection
request** (FriendCard + short note, size-capped, rate-limited per sender by
relayd); it lands in a requests inbox; nothing else flows until the user
accepts, and accepting runs the normal friending path. Onboarding win: one
person shares a single QR/link with a big group and accepts requests as they
arrive — no N×N mutual scanning, and the trust model stays intact.

Spec (`specs/open-relay-messaging.md`) must cover: addressing without the
family token (or an invite-token variant); relayd quota/abuse limits (reuse
the T4-09 admission machinery); inbox UX; interaction with consent-based
friend introductions (cc21573) — they may be the same surface; **and a
section on relay presence "Phase 2"** (from `CONNECTIVITY_INDICATOR.md`), so
all relay-server changes are reasoned about together and deployed once, with
David's explicit go-ahead.

---

## Blocked on David / devices

### T11 🔴 iPhone→Android friending doesn't reciprocate
No code until diagnosed; the handlers read correct. Device session script:
1. `adb logcat` filtered on
   `FriendRequest|Dropping envelope|Dropping friend request`.
2. Repro: iPhone scans the Android QR, confirms.
3. Branch: nothing logged → check iOS Console for "Friend request queued for
   later delivery" (kind=3 went to mules/relay); decode/parse errors →
   cross-platform encoding bug; identity-mismatch → card/key bug.
4. Code suspects in order: iOS relay fallback posting to its own family
   mailbox instead of the contact's; kind=3 excluded from digest resend so a
   lost first send never retries.

Checkable now, no device needed (~30 min): verify hidden kinds are included
in digest frames — that's suspect (4b) and would also explain other
one-shot-message losses.

### L3 + V1 — LAN field validation (one combined device session)
Agent prep: extract the L3 checklist (sweep timing, tie-break logs, hint
replay, VPN denied-counts) and the V1 gates from `specs/same-lan-transport.md`
into one printable one-pager with the exact logcat filter per check. Run each
check VPN-on and VPN-off (the Pixel's WireGuard full-tunnel skews results).
Piggyback the T15 phase-2 verification onto this session.

---

## Dropped

- **L5 — UDP multicast beacon.** Closed as won't-do: a third discovery
  protocol with a plaintext, fingerprintable beacon and permanent maintenance
  cost, justified only for networks that filter mDNS *and* defeat the subnet
  sweep *and* pass multicast. Revisit only if L3 field data shows that slice
  actually exists.
- **T7 as a standalone item.** Merged into T15 phase 1.
- **Relay presence "Phase 2" as a standalone item.** Folded into the T17 spec
  so relay-server changes ship as one reviewed deploy.
- **T17 "open delivery" variant.** Publishing agree keys to strangers and
  unsolicited message flow break the trust model and invite spam; the
  connection-request inbox above achieves the same onboarding goal.
