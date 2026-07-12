# 1:1 BLE muling — let nearby phones carry direct messages

**Status: implemented (2026-07-11, branch `agent/ble-1to1-muling`), pending
fleet verification.** Hooks A and B (§3) and the seenIds restart hardening
(§5) are coded and unit-tested (§7); the 3-phone fleet test plan in §7 has
not been run yet. Companion doc: `CONNECTIVITY_INDICATOR.md` (which assumes
this fix ships first — see §8). The receipt-muling follow-up (GitHub issue
#20) is now also implemented as Hook C (§6).

## 1. The bug (still open as of 2026-07-11)

Group messages flood to every connected BLE link and get carried/muled by
everyone. **1:1 messages do not.** `RealMeshSender.enqueueAuthored`
(`android/app/src/main/kotlin/com/cruisemesh/app/chat/MeshSender.kt:157`) does
exactly one BLE thing:

```kotlin
if (!MeshRouter.sendToUserId(contact.userId, encodeOutboundEnvelopeFrame(outbound))) {
    Log.i(TAG, "... not currently connected; message stays local until next digest sync")
}
```

So a 1:1 message reaches its recipient only via (a) a *direct* HELLO'd link to
that exact contact, (b) digest-sync resend when that contact directly
reconnects, or (c) relay upload when *we* have internet. Consequence: you're
on the pool deck with no internet, a family member with Wi-Fi is standing next
to you, your message to a third person strands on your phone — even though
that neighbor would have muled a *group* message instantly.

This was diagnosed 2026-07-11 from paired device logs and noted as "candidate
fix (not yet built)". It was never built. This doc is the build spec.

## 2. What already exists (the fix is small because of this)

The entire downstream mule pipeline already ships and works — it's exercised
today by group traffic and relay proxy-polling. Inventory, all in
`mesh/MeshService.kt`:

| Piece | Where | What it does |
|---|---|---|
| Inbound foreign-envelope handling | `processInboundEnvelope` (~1227) | A sealed envelope we can't open → `relayForeignEnvelope` (flood onward, TTL-bounded) + `carryForeignEnvelope` (persist in carry queue) |
| Carry queue | `carryForeignEnvelope` (~1309) | "Family" (hint matches a known contact/group) = kept until expiry; foreign shares `FOREIGN_CARRY_BUDGET_BYTES`; idempotent on `msg_id` |
| Hand-off to recipient | `drainCarriedEnvelopesTo` (~1380) | On HELLO from the recipient, send matching carried envelopes on that link, then drop them |
| Mule-to-mule spray | `sprayCarriedEnvelopesTo` (~1135) | On digest exchange, forward carried envelopes the peer doesn't already have (suppressed by the digest's carried-`msg_id` set) |
| Mule → relay uplink | `uploadFamilyCarriedEnvelopes` (~665) | An internet-connected mule uploads family-carried envelopes to the relay |
| Relay → mule | `carryRelayEnvelope` (~1349) + proxy hints | A phone with internet fetches contacts' relay mail and carries it over BLE (`enqueueRelayCarriedEnvelope`, `from_relay=1` never re-uploaded) |
| Dedupe layers | `GossipState.seenIds`, carry-queue `msg_id` idempotence, store `UNIQUE(chat_id, sender_user_id, lamport)`, relay `(family_token, msg_id)` | Redundant copies are harmless at every hop |

**The only missing link is at the source:** the sender never *gives* its own
authored 1:1 envelope to anyone except the recipient. Once a non-recipient
peer receives it, everything above takes over unmodified.

**Rollout property that falls out of this: only the sender needs the new
build.** Neighbors running today's APK already carry unopenable BLE envelopes
(`processInboundEnvelope` → `carryForeignEnvelope` is in shipped code). No
protocol change, no new frame types, no core/Rust changes required (§4).

## 3. The fix — two sender-side hooks

### 3.1 Hook A: send-time spray (immediacy)

In `RealMeshSender.enqueueAuthored` (`chat/MeshSender.kt`), when the direct
send misses, spray the same frame to every currently connected link:

```kotlin
val frame = encodeOutboundEnvelopeFrame(outbound)
if (!MeshRouter.sendToUserId(contact.userId, frame)) {
    val muled = MeshRouter.relayToAll(frame)
    Log.i(TAG, "$logLabel: ${contact.name} not connected; sprayed to $muled mule link(s), stays queued")
}
```

- `MeshRouter.relayToAll` already exists (`mesh/MeshRouter.kt:116`).
- Receiving peers can't open it (sealed to the recipient) → they flood + carry
  it. If the recipient is *their* contact too (the normal family case) it's a
  "family" carry: kept until expiry and **uploaded to the relay the moment the
  mule has internet** — this is exactly the "hop to a phone that relays to
  Wi-Fi" path.
- If zero links are up, `relayToAll` returns 0 and behavior is exactly today's.
- Do NOT spray when the direct send succeeded — the direct path + receipts +
  digest sync already cover that case; redundant copies would only burn
  neighbors' carry budgets.

### 3.2 Hook B: digest-time spray (catch-up for mules that arrive later)

Hook A only helps if a mule is connected at the moment of sending. A message
authored at 09:00 with nobody around must still get muled when a neighbor
walks by at 11:00. Add `sprayOwnPendingOutboundTo(address, peerUserId,
peerKnownMsgIds)` in `MeshService`, called from `handleDigest` right next to
the existing `sprayCarriedEnvelopesTo` call (~line 1058):

```
for each contact C in store.listContacts() where C.userId != peerUserId:
    deliveredThrough = store.receiptThrough(C.userId, identity.userId, RECEIPT_TYPE_DELIVERED)
    pending = store.outboundEnvelopesAfter(C.userId, identity.userId, deliveredThrough)
    for env in pending (oldest first):
        skip if env.expiry <= now
        skip if env.msgId ∈ peerKnownMsgIds        // peer already carries it
        skip if byte budget exhausted (see below)
        MeshRouter.sendToAddress(address, encodeOutboundEnvelopeFrame(env))
```

- Both store calls already exist in the FFI: `outboundEnvelopesAfter`
  (`core/src/store.rs:564`) and `receiptThrough`. "Pending" = above the peer's
  cumulative DELIVERED watermark — the same definition `nextAuthoredLamport`
  and the digest resend already use, so a delivered message stops being
  sprayed as soon as the receipt comes back by any path.
- `peerKnownMsgIds` is the digest's carried-`msg_id` set already parsed in
  `handleDigest` — the same suppression `sprayCarriedEnvelopesTo` uses, so a
  mule that already carries the envelope isn't re-sent it on every reconnect.
- Skip group-addressed envelopes (`recipientUserId` = a group id): group
  traffic already floods at send and sprays via the carried path.
- Budget: `OWN_OUTBOUND_SPRAY_BUDGET_BYTES = 256 * 1024` per digest exchange,
  oldest-first, stop when exceeded (attachment manifests can be large; the
  512-byte GATT frame cap means big envelopes fragment across many writes —
  see `FrameFraming`). Constant lives next to
  `DIGEST_CARRIED_MSG_IDS_LIMIT` in `MeshService.kt`.
- This runs for contact *and stranger* peers alike, matching
  `sprayCarriedEnvelopesTo` — a stranger CruiseMesh phone is still a valid
  best-effort mule (their side classifies it under the foreign budget).

### 3.3 Non-goals of this change

- No change to the direct-send path, digest resend, or relay upload.
- No new frame types, no HELLO changes, no relayd changes.
- Receipts are NOT muled by this change (see §6).

## 4. Core/Rust changes: none required

Hook A uses only existing router primitives. Hook B composes two existing
uniffi store calls per contact. At family scale (O(100) contacts, most with
nothing pending — `outboundEnvelopesAfter` returns empty), one digest exchange
costs ~100 cheap indexed queries; acceptable. If profiling ever disagrees, the
optimization is a single new store query (`pending_outbound_envelopes_all`
joining receipts), **not** part of this change.

## 5. Edge cases and why they're safe

- **Own envelope flooding back to us.** `buildOutboundAuthoredEnvelope`
  records our `msg_id` in `GossipState.seenIds` at authoring, so a mule
  flooding it back is dropped as SEEN. **But `seenIds` is in-memory**: after a
  process restart, our own envelope arriving from a mule would fail to open
  (it's sealed to the recipient) and we'd start "carrying" our own message —
  harmless (relay dedupes by `msg_id`; we'd effectively re-mule our own mail)
  but wasteful. Hardening, include it: on service start, seed
  `GossipState.seenIds` with `msg_id`s from `outbound_envelopes` and the
  carried queue (both enumerable via existing queries; if a bulk
  `outbound msg_id` listing is missing, add a tiny read-only store fn — the
  one permitted core addition).
- **Duplicate delivery to the recipient** (direct digest resend + a mule drain
  both fire): recipient's `seenIds` and the store's
  `UNIQUE(chat_id, sender_user_id, lamport)` both drop it. Already the norm
  for group traffic.
- **Mule uploads to relay + sender also uploads**: relay dedupes by
  `(family_token, msg_id)` (DESIGN.md §9). Already the norm for carried mail.
- **Privacy**: a mule receives the sealed ciphertext + the §6.4 public header
  it would have seen anyway had it been in flood range. No new exposure class;
  identical to group-flood semantics.
- **Hop budget**: envelopes carry `DEFAULT_HOP_TTL = 7`; the flood path
  already decrements/enforces it. Spray doesn't touch TTL.
- **Storage on mules**: family carries are unbounded-by-count but
  expiry-bounded (`defaultExpiry`); stranger carries share
  `FOREIGN_CARRY_BUDGET_BYTES`. Both existing policies, unchanged.
- **Friend requests (kind=3) and group invites (kind=4)** live in
  `outbound_envelopes` too and get muled by Hook B. That's desirable
  (friending someone who's out of range works via a mule) — no special-casing.

## 6. Receipt muling — implemented (2026-07-12, GitHub issue #20)

**Status: implemented (branch `agent/ble-receipt-muling`), pending fleet
verification.** Originally deferred (below) so message-path muling could be
validated on the fleet first; now built with the same shape as Hook B.

### 6.1 The gap it closes

After a mule delivers the message, the recipient's DELIVERED/READ receipt
previously travelled only via relay upload or a *direct* link back to the
sender. In a pure-offline A → mule → C hop the message arrives but the
sender's tick stays "sent" — delivery was fixed by §3, tick freshness was not.

### 6.2 The fix: Hook C (digest-time receipt spray)

`sprayOwnPendingReceiptsTo(address, peerUserId, peerKnownMsgIds)` in
`MeshService`, called from `handleDigest` right after `sprayOwnPendingOutboundTo`.
It offers the digest peer our own `store.pendingRelayOutgoingReceiptEnvelopes`
(the un-posted, unexpired receipt queue) so the peer can mule them back toward
the original message senders:

```
pending = store.pendingRelayOutgoingReceiptEnvelopes(RELAY_BATCH_LIMIT, now)
for env in pending (oldest first):
    skip if env.recipientUserId == peerUserId   // syncReceiptsFirst already sent it directly
    skip if env.expiry <= now
    skip if env.msgId ∈ peerKnownMsgIds          // peer already carries it
    skip if byte budget exhausted
    MeshRouter.sendToAddress(address, encodeOutgoingReceiptEnvelopeFrame(env))
```

- **No new machinery.** Receipt envelopes are *already* sealed to the sender
  with a recipient-keyed hint (`buildOutgoingReceiptEnvelope`), so a mule that
  isn't the sender treats them exactly like any other foreign envelope: can't
  open → floods + family-carries via the shipped `processInboundEnvelope` →
  `carryForeignEnvelope` path → drains on meeting the sender. No protocol, no
  frame type, no core/Rust change — same reuse property as Hook B.
- **Terminal condition:** `pendingRelayOutgoingReceiptEnvelopes` already
  excludes relay-posted and expired rows, so a receipt stops being sprayed
  once it reaches a relay (the durable path took over) or ages out.
- **Selection logic** is a pure, unit-tested function `OwnReceiptSpraySelector`
  (mirrors `OwnOutboundSpraySelector`): excludes the peer's own receipts,
  expired, peer-known msgIds; oldest-first byte budget
  `OWN_RECEIPT_SPRAY_BUDGET_BYTES = 64 KiB` (receipts are tiny — this is a
  backstop, not a normal-case limiter).
- **`seenIds` hardening reuse:** `buildOutgoingReceiptEnvelope` already records
  each receipt's `msgId` in `GossipState.seenIds`, so a mule flooding our own
  receipt back is dropped as SEEN (same protection as §5 for messages).

### 6.3 Testing

Unit: `OwnReceiptSpraySelectorTest` — excludes peer's own receipts; excludes
expired (boundary = expired); excludes peer-known msgIds; respects byte budget
oldest-first; empty when nothing pending. (Runs without the host core lib —
pure data-class construction, no FFI.)

Fleet (continues §7's scenario 2, the pure-offline hop): after B mules A's
message to C and C reads it, B should carry C's DELIVERED/READ receipt back to
A on their next reconnect — A's tick advances to delivered/read without any
phone touching the internet.

## 7. Testing

Unit:
- Extract Hook B's selection logic into a pure function (pattern:
  `ChatListLogic`/`nextAuthoredLamport`) taking
  `(contacts, peerUserId, peerKnownMsgIds, pendingByContact, nowMs, budget)`
  and returning the ordered envelope list. Test: excludes the peer's own chat;
  excludes expired; excludes peer-known msgIds; excludes group-addressed;
  respects byte budget oldest-first; empty when nothing pending.
- `MeshSender` test: direct-send failure triggers exactly one `relayToAll`
  with the identical frame bytes; direct-send success triggers none.

Fleet (3 phones: A = sender, B = mule, C = recipient — use the test-fleet IDs
from the debug notes, keep real userIds out of the repo):
1. **Live mule:** C out of range/Bluetooth-off, A and B adjacent, all
   airplane-mode except B on Wi-Fi. A sends 1:1 to C → B's log shows
   `Carrying foreign envelope (family=true)` then
   `Uploaded carried envelope ... to relay`. C on internet later → message
   arrives via relay.
2. **Pure offline hop:** all three offline; A near B (message sprays), then B
   walks to C (HELLO) → `Drained 1 carried envelope(s)` on B, message appears
   on C. Expect A's tick to stay "sent" until a receipt path exists (§6).
3. **Late mule (Hook B):** A sends with nobody around; B arrives 10 min later
   → digest exchange sprays it (`log: sprayed own pending`); continue as (1).
4. **No regression:** A↔C direct adjacent send still delivers instantly and
   no spray fires.
5. **Restart hardening:** after (1), force-stop and relaunch A's app,
   reconnect A↔B → A must not enqueue its own message as a carried envelope.

## 8. Impact on the connectivity indicator

`CONNECTIVITY_INDICATOR.md` is written against post-fix semantics: it includes
a `MESH_CARRY` reachability tier ("nearby phones can carry this message")
that is only honest once this ships. **Land this change before (or together
with) connectivity-indicator Phase 1**, or ship the indicator with
`MESH_CARRY` disabled.
