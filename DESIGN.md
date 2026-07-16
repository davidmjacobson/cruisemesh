# CruiseMesh — Design & Plan

*Offline-first family messaging for cruise ships. Delay-tolerant BLE mesh with an
optional internet relay, end-to-end encrypted.*

Status: draft v0.1 (2026-07-09)

---

## 1. Problem & goals

On a cruise ship there is no cellular service and Wi-Fi internet costs money per
device. A family scattered across a ship (pool deck, cabin, dining room) wants to
text each other "meet at the buffet at 6" and know whether the message got through.

**Goals (v1)**

- Text messaging between known contacts ("friends") and small groups, phone-to-phone,
  no internet required.
- Delay-tolerant delivery: messages queue and get relayed opportunistically; minutes
  of latency is acceptable and expected.
- Signal-style status ticks: sent ✓, delivered ✓✓, read (filled/blue ✓✓).
- End-to-end encryption of message contents **and** receipt metadata.
- Friending via out-of-band ID string or in-person QR code scan.
- Group chats.
- Internet-assisted delivery: when any phone gets internet (ship Wi-Fi package, port
  cellular), it flushes queued messages through a relay server.
- A relay daemon that runs on a cheap Linux VPS; self-hostable per family.
- Broadcast mode: unauthenticated local "shout" channel, Bridgefy/bitchat style.

**Explicit non-goals (v1)**

- General media attachments (photos, audio memos) — but the wire format and
  storage must leave room. Contact profile photos are a separate, tiny metadata
  path. §6.2.1 and §8.
- Anonymity / censorship resistance (Briar's threat model). Our adversary is "no
  internet," not a nation-state. We still encrypt end-to-end because relays and
  strangers' phones carry our ciphertext.
- Ship-wide stranger-to-stranger social features (beyond broadcast mode).
- Real-time anything: no typing indicators, no calls, no presence guarantees.

---

## 2. Prior art — and should we build at all?

| Project | What it is | Why it isn't the answer |
|---|---|---|
| **bitchat** | BLE mesh chat (Noise protocol, 7-hop TTL, Nostr internet fallback). Open source, actively used. | Deliberately ephemeral: no persistent contacts, no delivery/read receipts, no groups-of-friends model. Android build not on Play Store. **Best architectural reference** — its BLE GATT mesh design is exactly the transport we want. |
| **Briar** | P2P messaging over Bluetooth/Wi-Fi/Tor. Mature, security-audited. | **No iOS app and no plans for one** — iOS background restrictions make Briar's model infeasible there. A family app that excludes iPhones is dead on arrival. |
| **Berty / Wesh** | P2P messaging, iOS+Android, BLE + Multipeer + internet, IPFS-based. | Closest feature match on paper, but the protocol is admittedly partially implemented, the Android app has been pulled for security updates, and community reports question viability. Worth mining for lessons (especially its per-OS transport choices), not depending on. |
| **Bridgefy** | Commercial BLE mesh SDK + app; the "broadcast mode" inspiration. | Repeatedly broken by academic cryptanalysis ("Breaking Bridgefy, again" — misused libsignal), closed source. A cautionary tale: **use crypto libraries whole, don't assemble primitives.** |
| **Meshtastic** | LoRa mesh texting via ~$30 radio nodes + phone app. | **Actually works well on cruise ships** — LoRa penetrates where BLE dies. But it requires everyone to carry extra hardware and its default channels aren't E2EE to the phone. |

**Honest recommendation:** if the requirement were only "family texting on the next
cruise, minimum effort," the answer is **buy Meshtastic nodes** (or just pay for the
cruise line's messaging add-on). Nothing app-only does the full list (persistent
friends + receipts + groups + E2EE + iOS + relay server), which is exactly why
CruiseMesh is worth building — with bitchat as the transport-layer reference and
Bridgefy as the crypto anti-pattern.

---

## 3. The physics problem (read this before the architecture)

Cruise ships are Faraday cages subdivided by steel bulkheads. Real-world reports are
consistent: BLE gets 10–30 m line-of-sight, roughly one room otherwise, and a
ship-wide hobbyist mesh never reaches critical mass because everyone just buys Wi-Fi.

**Design consequence:** with only ~4–10 family installs on board, "mesh" is not a
connected graph. It is three delivery modes, in order of how often they'll fire:

1. **Direct contact** — you're within BLE range of the recipient. Instant.
2. **Data mule (store-carry-forward)** — a family member's phone picks up your queued
   message when you cross paths and physically carries it until it meets the
   recipient. This is classic delay-tolerant networking (DTN), and on a ship where
   everyone orbits the same buffet, it works better than it sounds.
3. **Internet relay** — any phone with a Wi-Fi package (or in port) syncs the whole
   family's queue through the relay server. One paid Wi-Fi device becomes the
   family's uplink.

Multi-hop flooding through *strangers'* phones (mode 4) is designed in (TTL-limited
gossip, same as bitchat) and costs nothing, but we must not depend on it for the
family use case. Every protocol decision below assumes hours-scale worst-case
latency, out-of-order arrival, and duplicate delivery.

---

## 4. System overview

```
┌─────────────── phone ───────────────┐
│  UI (chats, ticks, QR scan)         │
│  ────────────────────────────────   │
│  Core (shared library)              │
│   • identity & contacts             │
│   • E2EE (seal/open, group keys)    │
│   • message store + outbound queue  │
│   • sync/gossip engine (dedupe,     │
│     TTL, receipt generation)        │
│  ────────────────────────────────   │
│  Transports (pluggable)             │
│   • BLE GATT (primary)              │──── BLE ────  other phones
│   • Same-LAN Bonjour/NSD + TCP      │──── Wi-Fi ──  other phones
│   • Internet relay client (HTTPS/WS)│──── TLS ────┐
└─────────────────────────────────────┘             │
                                          ┌─────────▼──────────┐
                                          │ cruisemesh-relayd   │
                                          │ (Linux VPS, Docker) │
                                          │ mailbox of sealed   │
                                          │ envelopes, TTL 30d  │
                                          └────────────────────┘
```

Everything above the transport line is transport-agnostic: the sync engine hands
sealed envelopes to whatever links are up. Same-LAN TCP uses that seam without
changing crypto, storage, receipts, deduplication, or mule behavior; future transports
such as Wi-Fi Aware can do the same.

---

## 5. Transport layer

### 5.1 Why BLE (and not the alternatives)

- **BLE GATT** is the only radio that is cross-platform (iOS ↔ Android), works in
  the background on iOS (with real limitations, §12), and is battery-viable for
  all-day duty cycling. This is what bitchat ships on. **Primary transport.**
- **Apple Multipeer Connectivity**: great throughput, iOS-only. No.
- **Wi-Fi Direct / Wi-Fi Aware**: Android-only in practice; cross-OS pairing is a
  known tarpit (Berty maintains three separate transports because of this). Revisit
  only as a media-transfer fast path (§8).
- **Ship Wi-Fi LAN (no internet package)**: some ship networks allow associated
  clients to communicate locally without paid internet. CruiseMesh discovers peers
  with Bonjour/NSD, authenticates accepted contacts with Noise XX, and carries the
  existing mesh frames over TCP. Client-isolated networks simply leave this path
  unavailable; BLE and relay continue normally. See
  [`specs/same-lan-transport.md`](specs/same-lan-transport.md).

### 5.2 BLE roles and link protocol

Each phone runs **both** GATT roles simultaneously, bitchat-style:

- **Peripheral**: advertises a fixed CruiseMesh service UUID; exposes one write
  characteristic (inbound frames) and one notify characteristic (outbound frames).
- **Central**: scans for that service UUID (background-safe on iOS), connects,
  exchanges frames.

On connect, peers run a short sync handshake (§7.3), exchange frames, and stay
connected while in range. Frames are length-prefixed binary; envelopes larger than
negotiated MTU (~180–500 B typical) are fragmented at the link layer with a 2-byte
fragment header. Realistic throughput is single-digit KB/s — fine for text,
disqualifying for multi-hop media (§8).

### 5.3 Gossip / mesh relaying

- Every envelope has a random 16-byte `msg_id`. Each node keeps a seen-ID LRU
  (bloom-filter-backed, ~50k entries) and forwards each envelope at most once.
- `hop_ttl` starts at 7 (bitchat's number; plenty for a ship) and decrements per hop.
- `expiry` timestamp (default 7 days) after which carriers drop the envelope.
- **Carry queue**: nodes store envelopes addressed to known contacts/groups
  indefinitely until expiry, and a bounded budget (e.g. 5 MB) of foreign envelopes
  for altruistic muling. Family messages always win eviction fights.
- No routing tables, no path discovery. At family scale, epidemic flooding with
  dedupe is strictly better than anything cleverer.

---

## 6. Identity, friending, and encryption

### 6.1 Rules of engagement

Bridgefy got broken twice *after* adopting libsignal. The lesson: no bespoke
constructions. We use **libsodium** primitives whole, via its maintained bindings,
and the design must remain boring enough to describe in one page.

### 6.2 Identity

- A user identity = **Ed25519 signing keypair + X25519 encryption keypair**,
  generated on device, never leaves it (v1: no multi-device; §13).
- **UserID** = first 16 bytes of BLAKE2b(Ed25519 public key). Displayed/base32 for
  out-of-band sharing (`CM-K7QX-9M2P-...`).
- **Friending**: QR code (or pasted string) containing `{name, both public keys,
  optional relay URL, optional relay token}`. Scanning imports the contact and queues a signed
  friend-request envelope back; friendship is mutual once both sides hold each
  other's keys. Contact card shows a short fingerprint phrase (4 words) for verbal
  verification, Signal-safety-number style.
- **Friends-of-friends introductions** can reduce a connected family's
  physical setup from `N(N - 1) / 2` to `N - 1` QR scans. Public contact cards are
  suggested through named mutual friends; the user explicitly adds a suggestion,
  and the candidate's phone enforces its own default-on discovery setting. The
  protocol, privacy boundary, and rollout are specified in
  [`specs/friends-of-friends.md`](specs/friends-of-friends.md).

### 6.2.1 Contact profile photos

Profile photos are **not** general chat media. They are durable contact metadata:
"this is what I look like in your conversation list and friend card."

- Canonical form: square-cropped JPEG or WebP, max **256 x 256**, target
  **<= 24 KiB**. The app also derives a tiny thumbnail (**64 x 64**, target
  **<= 4 KiB**) for list views and friending previews.
- Storage: content-addressed by BLAKE2b hash, with a monotonic `avatar_epoch`
  timestamp so updates are replaceable and idempotent. Newest epoch wins.
- **QR friend cards do not embed image bytes.** They may carry
  `{avatar_hash?, avatar_epoch?}` so a newly scanned contact can tell whether
  an avatar exists, but the QR payload stays text-sized.
- **Friending exchanges full photos on the spot.** Friending is in-person, so
  a direct BLE link to the new friend is the normal case. When that link is
  live, each side's sealed `profile-sync` envelope carries the **full-size**
  avatar immediately — a 24 KiB transfer takes seconds, and both people walk
  away from the handshake with each other's actual photos.
- Fallbacks, for when the peers separate before the transfer completes (or a
  photo changes mid-cruise): the signed `friend-request` follow-up envelope may
  piggyback the thumbnail if it fits comfortably, and the queued `profile-sync`
  retries over direct BLE on the next encounter or over the internet relay.
- Full-size avatar bytes move only over **direct BLE** or the **internet
  relay**. They are small enough to transfer opportunistically, but they are
  **not** treated like foreign mule traffic and are never flood-gossiped to
  uninvolved strangers' phones.
- UI fallback remains the deterministic color + initials bubble when no shared
  photo is present, decoding fails, or the user intentionally keeps no photo.

### 6.3 Message encryption (1:1)

Per-message **sign-then-seal**:

1. Plaintext body (§7.1) is signed with sender's Ed25519 key.
2. Signed body is encrypted to the recipient's X25519 key with an ephemeral sender
   key — libsodium `crypto_box_seal` + embedded sender auth (i.e., HPKE-style ECIES).
3. Padded to the next 256-byte bucket before sealing, so relays can't distinguish
   "ok" from a paragraph.

**Deliberate trade-off:** no Double Ratchet in v1. Ratchets assume ordered-ish,
online-ish delivery; DTN gives us neither, and ratchet desync on a ship with no
side-channel to heal it means silently lost messages — the one failure mode this app
exists to prevent. Per-message ephemeral keys give confidentiality and sender-side
forward secrecy; we give up recipient-compromise forward secrecy. For "meet at the
buffet," robustness wins. The envelope has a `version` byte precisely so a
ratchet/PQ upgrade can ship later without a flag day.

### 6.4 What observers see

The public envelope header contains only: `version, msg_id, hop_ttl, expiry,
recipient_hint, ciphertext`. `recipient_hint` = 8-byte BLAKE2b(recipient UserID ‖
day-salt), where `day-salt` = the UTC day number
`timestamp_ms.div_euclid(86_400_000)` encoded as 8 big-endian bytes. That is
enough for relays/mules to route and recipients to cheaply test "for me?",
without a stable global identifier on the wire. Sender identity is **inside**
the ciphertext. Relay servers store sealed envelopes and hints, nothing else.

Relay presence (`POST /presence`) intentionally changes one relay-side
observable: a syncing phone may announce its own recent-day hints so friends can
see "online via relay." That lets the relay know "this connection currently
owns this rotating hint" and its online pattern. Hints still rotate daily, and
users can turn off "Share when I'm online"; querying friends' presence still
works when announcing is off.

### 6.5 Groups

- A group = ID + name + member list + a symmetric **group key** (XChaCha20-Poly1305).
- Creator generates the key and sends it to each member pairwise-sealed (§6.3), so a
  group costs N sealed invites, then one small envelope per message regardless of size.
- Group messages: signed by sender's Ed25519 key, encrypted with the group key,
  addressed with a group `recipient_hint`. Members mule for the whole group by default.
- Membership change ⇒ creator rotates the key and re-invites (a "remove" leaves the
  removed member able to read only pre-rotation traffic). Family-scale simplicity;
  no MLS.

### 6.6 Broadcast mode

A well-known "public" channel: envelopes signed but **encrypted with a fixed public
key** (i.e., readable by any CruiseMesh app), `recipient_hint` = broadcast constant,
flooded with normal TTL. UI labels it clearly as public-to-anyone-with-the-app.
Optional passworded channels later (Argon2id password → channel key, as bitchat does).

---

## 7. Messages, receipts, and the ticks

### 7.1 Plaintext body (inside the seal)

```
version | sender UserID | chat id (peer or group) | lamport counter |
timestamp | kind | payload
kinds: text=1, receipt=2, friend-request=3, group-invite=4,
       profile-sync=5, [reserved: attachment-manifest=16,
       attachment-chunk=17]
```

The per-chat **lamport counter** orders messages when clocks drift and lets a
recipient detect gaps ("message 12 arrived, 11 hasn't — keep waiting" shown as a
subtle gap indicator, not an error).

### 7.2 Receipts (the ✓✓)

| Tick | Meaning | Trigger |
|---|---|---|
| ✓ | *Sent* — sealed and handed to the sync engine (queued for mesh/relay) | local |
| ✓✓ | *Delivered* — recipient's device decrypted and stored it | delivery receipt |
| ✓✓ (filled) | *Read* — recipient viewed the chat | read receipt |

- Receipts are ordinary sealed envelopes (`kind=receipt`) — E2EE like everything
  else, so mules and relays learn nothing about read state.
- Each receipt is **cumulative**: "delivered/read through lamport N in chat C."
  Receipts are tiny, idempotent, and re-sent opportunistically on every peer sync, so
  a lost receipt heals itself — critical under DTN, and it means a single receipt
  envelope can confirm a whole backlog.
- In groups, ✓✓ = delivered to all members, filled = read by all (per-member detail
  on tap, like Signal). Expect group ticks to lag; the UI copy should normalize that.

### 7.3 Sync protocol (peer meets peer)

On BLE connect (or relay poll), peers exchange **digests**: per-chat (chat id,
highest-contiguous lamport, recent msg_id bloom filter). Each side then sends what
the other is missing, receipts first (they're smallest and unblock the most UI),
then messages oldest-first, then foreign mule traffic. Idempotent by msg_id, safe to
interrupt mid-transfer — reconnection just re-runs the digest exchange.

---

## 8. Media readiness (photos & audio memos, later)

Profile photos are the one intentional exception to "no media in v1": they are
small, durable contact metadata (§6.2.1), not chat attachments. Everything
larger than that still follows the rules below.

Decisions taken **now** so media doesn't force a redesign:

1. **Attachments are not messages.** A future photo message = a normal text-sized
   envelope carrying an *attachment manifest* (BLAKE2b content hash, size, mime,
   chunk count) + optional thumbnail. The conversation stays in order and receipts
   work unchanged; the blob is fetched separately.
2. **Content-addressed chunk store** on device, keyed by hash — dedupe for free, and
   any peer holding chunks can serve them.
3. **Chunks transfer only over fast/direct links**: single-hop BLE to the actual
   recipient (a 2 MB photo ≈ minutes at BLE speeds — acceptable directly, absurd
   multi-hop), internet relay, or a future Wi-Fi Aware/Multipeer fast path. Chunks
   are never gossiped/flooded.
4. Reserved `kind` values and the version byte (already in §7.1) cover the wire.

Nothing else about media gets designed today.

---

## 9. Relay server (`cruisemesh-relayd`)

A deliberately dumb mailbox:

- Single Go or Rust binary + SQLite, Docker image, runs on a $4/mo VPS.
- API (HTTPS + WebSocket): `POST /envelopes` and `GET /envelopes?hints=...`
  move the full **public** envelope header shape (`msg_id`, `hop_ttl`, `expiry`,
  `recipient_hint`, `sealed`) rather than plaintext message metadata; relay-side
  dedupe is by `(family_token, msg_id)`, fetch is by `recipient_hint` since cursor,
  delete-on-ack, 30-day retention, per-family auth token (baked into the QR friend
  card / group config) so randoms can't use your mailbox.
- Sees only sealed envelopes and hints (§6.4). A compromised relay learns traffic
  timing and approximate social graph size — not contents, senders, or read state.
- Phones poll it whenever internet appears and also push all queued outbound —
  including envelopes they're muling for family members, which is how one phone with
  a Wi-Fi package uplinks the whole family.

---

## 10. Client tech stack

Constraint: iOS + Android from day one (family reality), one developer, heavy
crypto/protocol logic that must behave identically on both platforms.

**Recommendation: Rust core + thin native shells.**

- **Core crate** (identity, sealing, store/SQLite, sync engine, framing) compiled for
  both platforms via **UniFFI**. One implementation of every subtle thing; testable
  headless on a desktop — including simulated 50-node DTN churn tests, which is how
  the sync engine gets trustworthy without two phones in hand.
- **Swift shell**: SwiftUI + CoreBluetooth. **Kotlin shell**: Compose + Android BLE.
  BLE APIs are so platform-idiosyncratic (especially iOS background behavior) that
  native code there is less work than fighting a cross-platform BLE plugin —
  the consistent failure theme in Flutter/RN mesh attempts.
- Precedent: Berty runs a Go core under native shells; bitchat is native Swift with
  a separate Android port (and its divergence bugs show why the shared core matters).

Alternative if Rust is unappealing: Kotlin Multiplatform (shared Kotlin core, same
native-BLE stance). Flutter/React Native are the fallback only if UI velocity
matters more than radio reliability — for this app it doesn't.

---

## 11. Milestones

| # | Milestone | Proves | Exit test |
|---|---|---|---|
| 0 | **Radio spike** (2 throwaway apps) | iPhone↔Android BLE exchange incl. both-backgrounded; range in steel buildings | 1 KB/min sustained with both apps backgrounded, 3 days battery sane |
| 1 | **Core + 1:1 direct** | Rust core, identity, QR friending, sealed text, ✓/✓✓/read over direct BLE | Two-phone family dogfood in the house |
| 2 | **DTN** | Carry queue, digests, dedupe, cumulative receipts, 3-phone mule delivery | Phone C carries A→B message between rooms; simulated 50-node churn test passes |
| 3 | **Relay** | `relayd` on a VPS, internet flush, mixed BLE+relay delivery with dedupe | Message delivered city-to-city; duplicates never render twice |
| 4 | **Groups + broadcast** | Group keys, rotation, per-member ticks; public channel | 4-person family group; broadcast between two unfriended installs |
| 5 | **🚢 Field test** | Everything, on an actual cruise | Family uses it for a week; log delivery latency, battery, mode mix (direct/mule/relay); probe ship-LAN client isolation while aboard |
| 6 | Media (per §8) | — | after the field test says the foundation holds |

Milestone 0 is the go/no-go gate: it de-risks the only thing we can't design around
(iOS background BLE) before any real investment.

## 12. Top risks

1. **iOS background BLE** — the existential one. Backgrounded iOS can still scan for
   a specific service UUID and accept connections, but slowly, and iOS may kill the
   app anyway. Mitigations: state restoration, both-role operation so Android
   centrals can wake iOS peripherals, and UX honesty ("open the app when you sit
   down"). If M0 shows backgrounded-iPhone↔backgrounded-iPhone fails, the app
   survives — that pair just syncs on foreground — but expectations must be set.
2. **Steel ship RF** — already priced in (§3): the design leans on mule + relay, not
   range. Field test measures which mode actually delivers.
3. **App distribution** — TestFlight + sideload/Play internal testing is fine for
   family; store review (Apple may poke at BLE background modes) only matters if
   this is ever distributed beyond family.
4. **Battery** — duty-cycle scanning (e.g. 10 s scan / 50 s idle when on battery,
   aggressive when charging). Budget: <5%/day radio overhead, measured in M0.
5. **Crypto review** — before any non-family distribution, pay for a design review.
   Bridgefy shipped first and got dissected twice.

## 13. Open questions (deliberately deferred)

- Multi-device / identity backup (v1: keys live and die on one phone; QR re-friend
  after a lost phone).
- Message history sync for a group member who joins late.
- Ratchet / post-quantum upgrade timing (envelope `version` byte reserves the path).
- Passworded broadcast channels; relay federation.

---

## 14. UI / UX plan (Android v1)

The current home screen is a Milestone-1 debug artifact — an identity card with
stacked buttons. This section is the target UI. The model is **Signal**: the home
screen is the conversation list, because opening a conversation is the only thing a
user launches this app to do. Identity, friending, and mesh plumbing all move behind
it. Material 3 components, Signal's information architecture.

### 14.1 Navigation map

```
Home (conversation list)
 ├─ tap row ───────────────── Chat
 ├─ FAB ✏ (compose) ───────── New chat = friends list
 │                              ├─ tap friend → Chat
 │                              ├─ "Add a friend" → QR scan
 │                              └─ "My friend card" → QR display
 ├─ avatar (top-left) ─────── Profile & settings
 └─ mesh status pill ──────── start mesh / explain current state
```

Friends do **not** get a home-screen tab. At family scale (4–10 contacts) a
dedicated Friends tab is dead weight you'd visit twice a cruise. Signal's own
pattern — contacts live one tap away behind the compose FAB, with friend management
(add / my card) at the top of that list — is the right shape here. A duplicate
"Friends" entry in the top-bar overflow menu covers discoverability.

### 14.2 Home: conversation list

One row per chat (1:1 now; groups and broadcast slot in later, §14.6), sorted by
last activity, newest first.

- **Avatar bubble**: shared contact photo when present; otherwise a colored
  circle whose hue is derived deterministically from the contact's UserID bytes,
  with initials from their display name (fallback: first two characters of the
  base32 ID). Shared avatars follow the tiny-metadata rules in §6.2.1, not the
  general media rules in §8.
- **Row content**: display name (bold when unread), one-line ellipsized snippet of
  the last message (`You: ` prefix for own), relative timestamp Signal-style
  (time-of-day today, weekday within 7 days, date otherwise), unread-count badge,
  and the ✓/✓✓/read tick when the last message is our own.
- **Unread count** is computable client-side without schema changes: our own read
  watermark for a chat is the outgoing READ receipt's through-lamport
  (`outgoing_receipt_through`); unread = peer messages with lamport above it.
- **Empty state**: friendly copy + a prominent "Add a friend" button — this is the
  first-run experience.
- **Long-press row** → delete conversation (the existing contact-deletion flow).

### 14.3 Mesh status — the one element Signal doesn't have

Where Signal shows "Connecting…" under the title, we show a persistent status pill
below the top bar: `Mesh off` / `Starting…` / `Meshing · N nearby` /
`Paused — Bluetooth audio` / `Syncing via relay`. Tapping it starts the mesh
(running the existing permission + battery-exemption flow) or explains the current
state in a sheet. The mesh **auto-starts on app open** once permissions have been
granted before — the giant "Start mesh" button dies. Honest copy matters here
(§12.1): the pill is where "open the app when you sit down" expectations get set.

### 14.4 Profile & settings (top-left avatar, like Signal)

Absorbs the old identity screen: editable display name, local/shared profile
photo picker, my QR friend card, UserID + fingerprint words (with the "read
these aloud to verify" hint), mesh on/off, relay configuration status. Nothing
here is daily-use, which is exactly why it lives behind the avatar.

### 14.5 Chat screen (already mostly built — polish only)

Keep the current push-based screen. Polish list: day separators, the §7.1
lamport-gap indicator ("some messages may still be in transit" — subtle, not an
error), tick-state legend on tap, bubble palette coherent with the avatar hue.

### 14.6 Later, designed for now

- **Groups (M4)**: "New group" action alongside the friends list under the FAB;
  group rows use the same bubble with a group glyph; per-member receipt detail on
  tap in-chat (§7.2).
- **Broadcast (M4)**: a clearly labeled "Ship broadcast" row pinned at the bottom of
  home, visually distinct (public-to-anyone framing per §6.6), collapsed by default.

Both slot into the §14.2 list without re-architecting the home screen.

---

## References

- bitchat — BLE mesh design reference: [github.com/permissionlesstech/bitchat](https://github.com/jackjackbits/bitchat) · [overview](https://www.techtarget.com/whatis/feature/What-is-Bitchat) · [Wikipedia](https://en.wikipedia.org/wiki/BitChat)
- Briar (no iOS, threat model contrast): [briarproject.org](https://briarproject.org/) · [iOS issue #445](https://code.briarproject.org/briar/briar/-/issues/445)
- Berty / Wesh protocol (transport lessons): [berty.tech/docs/protocol](https://berty.tech/docs/protocol/) · [github.com/berty/berty](https://github.com/berty/berty)
- Bridgefy cryptanalysis (what not to do): [Breaking Bridgefy, again (USENIX '22)](https://www.usenix.org/conference/usenixsecurity22/presentation/albrecht)
- Meshtastic (hardware alternative that works on ships): [beginner's guide](https://www.elecrow.com/blog/texting-without-cell-service-an-absolute-beginners-guide-to-meshtastic.html)
- Cruise connectivity / RF reality: [Seafy: cruise ship connectivity](https://seafy.com/en/blog/tech-wifi/cruise-ship-connectivity-explained-what-passengers-need-to-k)
