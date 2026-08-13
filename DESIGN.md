# CruiseMesh — Design

*Offline-first family messaging for cruise ships. Delay-tolerant BLE mesh with an
optional internet relay, end-to-end encrypted.*

This document explains the architecture and the reasoning behind it. For current
implementation status, see [ROADMAP.md](ROADMAP.md).

**Naming note:** this is the technical document, so it says *relay*, *token*,
and *mailbox*. Consumer surfaces (apps, website, email) deliberately never do —
there the feature is **internet delivery**, the hosted option is **Cruise
Pass**, and the credential is a **setup card** (`CMRELAY1:`). Same objects,
two vocabularies, on purpose.

Last updated: 2026-08-03

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
- ~~Broadcast mode: unauthenticated local "shout" channel, Bridgefy/bitchat
  style.~~ Dropped as a goal; see §6.6.

**Explicit non-goals (v1)**

- General media attachments (photos, audio memos) — but the wire format and
  storage must leave room. Contact profile photos are a separate, tiny metadata
  path. §6.2.1 and §8. *(Partly overtaken by events: small photos and voice
  memos now travel inline inside the sealed envelope, §8. What stays out of
  v1 is media too large to inline, which needs the manifest/chunk machinery.)*
- Anonymity / censorship resistance (Briar's threat model). Our adversary is "no
  internet," not a nation-state. We still encrypt end-to-end because relays and
  strangers' phones carry our ciphertext.
- Ship-wide stranger-to-stranger social features. (Broadcast mode was once the
  one exception to this; it no longer is. See §6.6.)
- Real-time anything: no typing indicators, no calls, no presence guarantees.

---

## 2. Prior art — and why build at all?

| Project | What it is | Why it isn't the answer |
|---|---|---|
| **bitchat** | BLE mesh chat (Noise protocol, 7-hop TTL, Nostr internet fallback). Open source, actively used. | Deliberately ephemeral: no persistent contacts, no delivery/read receipts, no groups-of-friends model. Android build not on Play Store. **Best architectural reference** — its BLE GATT mesh design is exactly the transport we want. |
| **Briar** | P2P messaging over Bluetooth/Wi-Fi/Tor. Mature, security-audited. | **No iOS app and no plans for one** — iOS background restrictions make Briar's model infeasible there. A family app that excludes iPhones is dead on arrival. |
| **Berty / Wesh** | P2P messaging, iOS+Android, BLE + Multipeer + internet, IPFS-based. | Closest feature match on paper, but the protocol is admittedly partially implemented, the Android app has been pulled for security updates, and community reports question viability. Worth mining for lessons (especially its per-OS transport choices), not depending on. |
| **Bridgefy** | Commercial BLE mesh SDK + app; the "broadcast mode" inspiration. | Repeatedly broken by academic cryptanalysis ("Breaking Bridgefy, again" — misused libsignal), closed source. A cautionary tale: **use crypto libraries whole, don't assemble primitives.** |
| **Meshtastic** | LoRa mesh texting via ~$30 radio nodes + phone app. | **Actually works well on cruise ships** — LoRa penetrates where BLE dies. But it requires everyone to carry extra hardware and its default channels aren't E2EE to the phone. |

**The honest assessment before writing any code:** if the requirement had only been
"family texting on the next cruise, minimum effort," the answer was to buy
Meshtastic nodes (or just pay for the cruise line's messaging add-on). But nothing
app-only does the full list — persistent friends + receipts + groups + E2EE + iOS +
relay server — which is why CruiseMesh exists, with bitchat as the transport-layer
reference and Bridgefy as the crypto anti-pattern.

---

## 3. The physics problem (read this before the architecture)

Cruise ships are Faraday cages subdivided by steel bulkheads. Real-world reports are
consistent: BLE gets 10–30 m line-of-sight, roughly one room otherwise, and a
ship-wide hobbyist mesh never reaches critical mass because everyone just buys Wi-Fi.

**Design consequence:** with only ~4–10 family installs on board, "mesh" is not a
connected graph. It is four delivery modes, and which one carries the traffic
depends on the ship:

1. **Same-LAN over ship Wi-Fi** — the one radio network that already blankets
   every deck is the ship's own Wi-Fi, and phones can associate to it without
   buying an internet package. Where the network doesn't isolate clients from
   each other, two associated phones reach each other directly over TCP —
   instant, cross-ship, and free. This sidesteps the BLE physics entirely by
   riding the ship's existing solution to the propagation problem.
   Field-validated; see §5.4.
2. **Direct contact** — you're within BLE range of the recipient. Instant.
3. **Data mule (store-carry-forward)** — a family member's phone picks up your queued
   message when you cross paths and physically carries it until it meets the
   recipient. This is classic delay-tolerant networking (DTN), and on a ship where
   everyone orbits the same buffet, it works better than it sounds.
4. **Internet relay** — any phone with a Wi-Fi package (or in port) syncs the whole
   family's queue through the relay server. One paid Wi-Fi device becomes the
   family's uplink.

Multi-hop flooding through *strangers'* phones (mode 5) is designed in (TTL-limited
gossip, same as bitchat) and costs nothing, but we must not depend on it for the
family use case. Every protocol decision below assumes hours-scale worst-case
latency, out-of-order arrival, and duplicate delivery — and treats anything
faster (a non-isolated LAN, a direct BLE link) as a welcome upgrade, not an
assumption.

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

A third client shares the same core. `desktop/` is a Windows build in two
pieces: a tray node that owns identity, SQLite, LAN, relay, and carry, and a
messenger window that talks to it over a named pipe. Being Rust itself, it
links `cruisemesh-core` directly rather than through UniFFI, so it exercises
the same store and sync engine the phones do without a binding layer in
between. It has no BLE — on a laptop the LAN and relay transports carry
everything. It is dogfood-only so far; §10 has the stack.

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
- **Ship Wi-Fi LAN (no internet package)**: where the ship network lets
  associated clients talk to each other, this is the best transport on board —
  promoted to its own section, §5.4.

### 5.2 BLE roles and link protocol

Each phone runs **both** GATT roles simultaneously, bitchat-style:

- **Peripheral**: advertises a fixed CruiseMesh service UUID; exposes one write
  characteristic (inbound frames) and one notify characteristic (outbound frames).
- **Central**: scans for that service UUID (background-safe on iOS), connects,
  exchanges frames.

On connect, peers run a short sync handshake (§7.3), exchange frames, and stay
connected while in range. Frames are length-prefixed binary; envelopes larger than
negotiated MTU (~180–500 B typical) are fragmented at the link layer with a
four-byte fragment header (`index16 | total16`, big-endian — `core/src/framing.rs`).
Realistic throughput is single-digit KB/s — fine for text, disqualifying for
multi-hop media (§8).

### 5.3 Gossip / mesh relaying

- Every envelope has a random 16-byte `msg_id`. Each node keeps a bounded
  seen-ID set (~50k entries) and forwards each envelope at most once.
  It is an **exact** set, not the bloom filter an earlier draft of this
  document specified: at family scale 50k exact 16-byte ids cost well under a
  megabyte, and an exact set can never false-positive-drop a genuinely new
  message. Eviction is FIFO rather than true access-ordered LRU — once an id
  is seen the frame is dropped and the id is never touched again, so
  recency-of-use and recency-of-insertion coincide here. `core/src/gossip.rs`
  carries the full argument.
- `hop_ttl` starts at 7 (bitchat's number; plenty for a ship) and decrements per hop.
- `expiry` timestamp (default 7 days) after which carriers drop the envelope.
- **Carry queue**: nodes store envelopes addressed to known contacts/groups
  indefinitely until expiry, and mule foreign envelopes within a bounded total
  carry budget (64 MB, `core/src/store.rs`). Family messages always win
  eviction fights.
- No routing tables, no path discovery. At family scale, epidemic flooding with
  dedupe is strictly better than anything cleverer.

### 5.4 Same-LAN transport over ship Wi-Fi (field-validated)

The BLE-centric analysis in §3 has one loophole in the app's favor: the ship
already solved the propagation problem. Access points cover every deck, and a
phone can associate to ship Wi-Fi without paying for internet. Where the network
does not isolate clients from each other, two associated CruiseMesh phones talk
directly: peers are discovered with Bonjour/NSD plus a bounded subnet sweep,
accepted contacts authenticate with Noise XX, and the existing sealed mesh
frames run over TCP. Crypto, storage, receipts, dedupe, and mule behavior are
unchanged — the LAN is just another link under the §4 seam.
[`specs/same-lan-transport.md`](specs/same-lan-transport.md) has the protocol
detail.

**Field results (Norwegian Jade, 2026):** clients on the ship network were
*not* isolated, which turned the LAN into effectively instant cross-ship
delivery between any two associated phones — the dominant transport for the
sailing. Two caveats from the same trip:

- **Holding the association is the hard part.** Some phones dropped the
  internet-less Wi-Fi after roughly an hour (captive-portal session timeouts;
  Android's adaptive connectivity preferring mobile data). BLE earned its keep
  as the always-on supplement precisely when a phone fell off the LAN.
- **Client isolation varies per ship.** Whether associated devices can reach
  each other varies by cruise line — and probably ship to ship within a line.
  This is priced in, not a threat to the product: CruiseMesh probes it
  automatically (the subnet-sweep verdict is surfaced in Connection details)
  and falls back to BLE + relay on isolated networks, so messaging works
  either way; the LAN is the speed upgrade, not the requirement. Collecting
  isolation reports across ships and lines — so a family knows before sailing
  which delivery mix to expect — is an open project goal; the 🚢 field-report
  issue template asks for exactly this.

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

### 6.6 Broadcast mode — designed, not planned for release

The original design: a well-known "public" channel, envelopes signed but
**encrypted with a fixed public key** (i.e., readable by any CruiseMesh app),
`recipient_hint` = broadcast constant, flooded with normal TTL, labeled in the
UI as public-to-anyone-with-the-app.

**This is no longer planned.** A channel any stranger can post to is a
moderation surface that a family messenger with one maintainer should not
grow, and it buys nothing for the use case the product is actually for. The
one variant still under consideration is a broadcast **scoped to a single
Shore Pass** — everyone on one pass can post to a shared channel, nobody
outside it can — which inherits the boundary the product already draws around
a family and is not open to strangers at all. That variant isn't specified
yet; if it happens it gets its own spec.

The wire design above is kept here because the `recipient_hint` constant and
the fixed-key construction would be reused by the pass-scoped version, and
because "why isn't there a public channel" is a reasonable question to have an
answer to.

---

## 7. Messages, receipts, and the ticks

### 7.1 Plaintext body (inside the seal)

```
version | sender UserID | chat id (peer or group) | lamport counter |
timestamp | kind | payload
kinds: text=1, receipt=2, friend-request=3, group-invite=4,
       profile-sync=5, friend-directory=6,
       introduced-friend-request=7, lan-endpoint-hint=8,
       relay-update=9, attachment-manifest=16,
       reaction=18, group-metadata-update=19,
       [reserved: attachment-chunk=17]
```

The allocation lives in `core/src/protocol.rs` (`KIND_*`); that file is
authoritative if this list ever drifts from it.

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

On BLE connect (or relay poll), peers exchange **digests**. One digest frame
covers one chat: the chat id, then one entry per sender in that chat —
`(sender_user_id, through_lamport)`, meaning "I have this sender's messages
contiguously through this lamport" — then an exact list of recent `msg_id`s
(a count followed by the 16-byte ids). The recent-id component is an exact
list rather than the bloom filter an earlier draft specified, for the same
reason §5.3 gives. `core/src/protocol.rs` documents the byte layout. Each
side then sends what
the other is missing, receipts first (they're smallest and unblock the most UI),
then messages oldest-first, then foreign mule traffic. Idempotent by msg_id, safe to
interrupt mid-transfer — reconnection just re-runs the digest exchange.

---

## 8. Media readiness (photos & audio memos, later)

**Shipped:** small photo/audio attachments travel inline inside the sealed
envelope itself — `core/src/content.rs`'s `CoreAttachmentPayload`, capped at
`ATTACHMENT_MAX_BLOB_BYTES` (180 KiB) after encoding. Because they're just
another envelope payload, they flow over **any transport**, including the
internet relay (relayd's per-family storage quota is explicitly sized around
inline photo/audio traffic — see `relayd/DEPLOY.md` §10). This covers the
common case (a compressed cruise photo) without the manifest/chunk machinery
below, which remains the design for media too large to inline.

Profile photos are the one intentional exception to "no media in v1": they are
small, durable contact metadata (§6.2.1), not chat attachments. Everything
larger than the inline cap still follows the rules below.

Decisions taken **now** so larger media doesn't force a redesign:

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

- A single Rust binary + SQLite, shipped as a Docker image; runs on a $4/mo VPS.
- API (HTTPS + WebSocket): `POST /envelopes` and `GET /envelopes?hints=...`
  move the full **public** envelope header shape (`msg_id`, `hop_ttl`, `expiry`,
  `recipient_hint`, `sealed`) rather than plaintext message metadata; relay-side
  dedupe is by `(family_token, msg_id)`, fetch is by `recipient_hint` since cursor,
  delete-on-ack, 30-day retention.
- The rest of the surface, all of it as dumb as the mailbox itself
  (`relayd/src/lib.rs`):

  | Route | What it does |
  |---|---|
  | `POST /envelopes/ack` | Acknowledges and deletes fetched envelopes, subject to §9's ack rules. |
  | `GET /ws` | Push channel — tells a client mail arrived so it need not poll. |
  | `POST /presence` | Announces and queries opaque presence blobs, so a phone can tell whether a contact has been reachable through this relay recently. |
  | `PUT /push/registrations` | Registers an APNs token so a backgrounded iPhone can be woken for mail. The relay learns a device token; it still cannot read a byte of content. |
  | `GET /healthz` | Liveness, unauthenticated. |
  | `/admin/families`, `/admin/families/{token}` | Provision, list, inspect, patch, and delete family tokens. Admin-credentialed, and how the hosted passes are minted — see `tools/relay_admin.sh`. |

  Presence and push registration are the two places the relay holds anything
  beyond sealed mail, which is why both are scoped per family token and expire.
- **A fetch page is bounded by bytes as well as rows.** `limit=` caps the row
  count, but one `sealed` payload may be 512 KiB, so a row-counted window over
  a backlog of large attachments can exceed what a client will decode — and
  because the next poll asks for the same window from the same cursor, that
  mailbox stalls permanently. The server therefore stops filling a page once
  its cumulative sealed bytes would push the response past the client's cap,
  always returning at least one row so an oversized envelope is never
  unreachable. Consequently **a short page is never end-of-mailbox**: both
  shells end a walk only on an *empty* page, and a client that meets an
  oversize page anyway (a self-hosted relay on an older build) halves `limit=`
  and retries the same cursor.
- **Two credential classes per family** (the SMTP/IMAP split — see §9.2): the
  **member token** authorizes everything (post, fetch, ack, WebSocket) and
  rides only the family's own setup card; the **deposit token** is post-only
  into that family's mailbox, under a tighter rate limit, and is what friend
  cards carry. Enforcement lives at relayd's auth layer: a deposit token
  presented to fetch/ack/WS gets a structured 403, so a leaked friend card is
  a nuisance (someone can stuff the mail slot, rate-limited), never a
  compromise (nobody can drain the mailbox).
- **Multi-tenant service hardening**, because the hosted instance sells Cruise
  Pass to strangers: a families table with per-family quota (256 MiB default),
  plan expiry and suspension with a 7-day grace window, per-token
  request/byte rate limits plus a global backstop, a per-token WebSocket cap,
  and an admin API that Shore Pass provisioning drives. Self-hosting stays
  free and runs the identical binary.
- Sees only sealed envelopes and hints (§6.4). A compromised relay learns traffic
  timing and approximate social graph size — not contents, senders, or read state.
- Phones poll it whenever internet appears and also push all queued outbound —
  including envelopes they're muling for family members, which is how one phone with
  a Wi-Fi package uplinks the whole family.
- **A row is deleted only by the phone that was its sole reader.** The ack
  rule is deliberately narrow: a phone may delete a relay row only when it can
  prove *it* was the envelope's sole true endpoint consumer, and when it
  cannot prove that, it leaves the row alone. Re-fetching costs bandwidth; a
  wrong delete costs someone their message. Proof comes from one of two
  places. Chat messages leave a durable local row keyed by the envelope's
  `msg_id`, so the phone can ask its own store "did I store this exact
  envelope, as a 1:1 message from someone else?". Everything else — receipts
  (two per message, and so the highest-volume traffic on the wire), profile
  sync, friend requests and directory updates, group invites, LAN endpoint
  hints, relay-change notices — persists no such row, so the phone instead
  records the `msg_id` in a small dedicated set at the moment it opens the
  envelope with its own key and consumes it. That set expires with the
  envelopes it describes, so it stays bounded without any upkeep of its own.
  Without it, those rows were undeletable *even though they had already been
  delivered over Bluetooth* — which is most of what a real mailbox fills up
  with. Still never acked, on purpose: anything merely muled for someone else
  (the relay copy is that person's durable fallback), anything opened with a
  shared group key or addressed to a group's shared hint (every member fetches
  that row), a phone's own outbound copy echoing back (it exists for the
  recipient), and anything whose local storage failed (it must be re-presented
  and retried).
- **Clients keep a fetch frontier, and sweep occasionally.** Most rows in a
  real mailbox are deliberately never acked — a proxy-fetched copy stays as the
  durable fallback, a legacy group-hint row is never acked at all — so the
  mailbox grows and, because rows come back in ascending id order, the newest
  message is the *last* one a walk from 0 reaches. A phone therefore remembers
  how far it got (per mailbox, keyed by a hash of URL + token, in its own
  store) and resumes there. The frontier only moves past a page whose envelopes
  all reached a terminal disposition and whose acks landed — the ack-safety
  rule applied to *skipping* rather than to deleting. At cold start and every
  six hours a pass walks from 0 again, so the rows that are supposed to stay
  put remain re-discoverable and a rebuilt relay (row ids restarting at 1)
  heals itself. The frontier is local-only and is stripped from `.cmbak`
  backups: it is a claim about a remote mailbox's *current* state, and a
  restore is exactly where that claim stops being true.

### 9.1 Mailbox routing (which relay serves which envelope)

Relay config belongs to the **relationship, not the device**. Every friend
card can carry its owner's relay URL + **deposit token** — "the mail slot
where letters for me get dropped" — and a phone's own saved config (member
token) is just its family's default box. Both shells share one core policy
(`resolved_contact_relay`): an envelope addressed to a contact goes to **that
contact's** card relay when the card has one, else to the sender's own config.
On the fetch side, a phone reads its **own** mailbox with its member token;
mail for you lands in your box because senders deposit into it. (Legacy
full-token friend cards from before the credential split still work — they
are accepted for deposit, and during the format transition they also permit
the old cross-mailbox polling; re-sharing a card upgrades it to
deposit-only.)

No family ever configures two relays; multi-mailbox behavior is *emergent*
from two real situations. (1) **Token rotation**: during a family's token
change, old- and new-token phones are effectively on different mailboxes —
cross-polling lets the fleet self-heal instead of silently splitting. (2)
**Friendships that span families**: two families who met aboard each hold
their own token, so staying in touch after the cruise requires posting into
the *recipient's* family mailbox (relayd scopes every row per token). Within
one family everything collapses back to a single relay — every contact
resolves to the same config and the distinct set dedupes to one, so there is
no extra traffic. A 401/403 from the phone's *own* saved token is surfaced as
"relay token rejected" rather than a generic failure, because a stale rotated
token is otherwise indistinguishable from an outage.

### 9.2 Relationship to email (the mental model)

The relay is a mail server, deliberately. Store-and-forward delivery,
per-family mailboxes, retry until delivered: that is SMTP's contract, and
CruiseMesh keeps the parts of email that four decades proved out.

- **The credential split is SMTP vs IMAP.** Email got one thing exactly
  right: anyone may *submit* mail to your server, but only you hold the
  credentials to *read and delete* it. The member/deposit token classes in §9
  are that split. A friend card is submission rights — the holder can deliver
  mail to your family and nothing else. The member token is the IMAP side,
  and it never leaves your family's phones.
- **The "MX record" is not public.** Email publishes the route to your
  mailbox in DNS, attached to a globally guessable address — which is why
  spam exists: the whole world holds submission rights to every mailbox.
  CruiseMesh's route travels only inside a mutually exchanged, key-bound
  friend card. Nobody can be *discovered*, only *introduced* — so
  capability-gated deposit plus rate limits do the job that email needed
  decades of filtering to approximate.
- **The mailbox is not the identity.** An email address *is* the identity,
  which is why changing providers is painful. Here identity is the keypair
  (§6.2) and the relay is one delivery route of four; a family can change
  relays and nothing about who they are changes — friend cards carry the new
  route on the next share.
- **The server is blind.** Every email server reads at least headers and the
  social graph, and usually content. relayd holds sealed envelopes and hints
  (§6.4); it cannot read contents, sender identities, or read state.
- **The server is optional.** Email has exactly one transport. Here the same
  sealed envelope travels over BLE, ship LAN, or in a family member's pocket,
  and the relay exists only for the cases where physics offers nothing
  better.

Shortest version, for a technical reader placing this in their mental map: a
mail server for your family, except the address is unguessable, the server
cannot read anything, and it is only consulted when the phones cannot reach
each other directly.

---

## 10. Client tech stack

Constraint: iOS + Android from day one (family reality), one developer, heavy
crypto/protocol logic that must behave identically on both platforms.

**Architecture: a Rust core with thin native shells.**

- **Core crate** (identity, sealing, store/SQLite, sync engine, framing) compiled for
  both platforms via **UniFFI**. One implementation of every subtle thing; testable
  headless on a desktop — including simulated 50-node DTN churn tests, which is how
  the sync engine gets trustworthy without two phones in hand.
- **Swift shell**: SwiftUI + CoreBluetooth. **Kotlin shell**: Compose + Android BLE.
  BLE APIs are so platform-idiosyncratic (especially iOS background behavior) that
  native code there is less work than fighting a cross-platform BLE plugin —
  the consistent failure theme in Flutter/RN mesh attempts.
- **Windows shell** (`desktop/`, §4): Rust throughout, so it links the core
  crate directly and skips UniFFI entirely — a tray node process plus a
  messenger window over a named pipe. No BLE; LAN and relay carry everything.
  Dogfood only.
- Precedent: Berty runs a Go core under native shells; bitchat is native Swift with
  a separate Android port (and its divergence bugs show why the shared core matters).

---

## 11. Milestones

| # | Milestone | Proves | Exit test | Status |
|---|---|---|---|---|
| 0 | **Radio spike** (2 throwaway apps) | iPhone↔Android BLE exchange incl. both-backgrounded; range in steel buildings | 1 KB/min sustained with both apps backgrounded, 3 days battery sane | ✅ Done |
| 1 | **Core + 1:1 direct** | Rust core, identity, QR friending, sealed text, ✓/✓✓/read over direct BLE | Two-phone family dogfood in the house | ✅ Done |
| 2 | **DTN** | Carry queue, digests, dedupe, cumulative receipts, 3-phone mule delivery | Phone C carries A→B message between rooms; simulated 50-node churn test passes | ✅ Done |
| 3 | **Relay** | `relayd` on a VPS, internet flush, mixed BLE+relay delivery with dedupe | Message delivered city-to-city; duplicates never render twice | ✅ Done |
| 4 | **Groups** (was: groups + broadcast) | Group keys, rotation, per-member ticks | 4-person family group | 🔨 Groups shipped; membership enforcement pinned by tests. Per-member read aggregation still open. Broadcast dropped from the milestone, §6.6. |
| 5 | **🚢 Field test** | Everything, on an actual cruise | Family uses it for a week; log delivery latency, battery, mode mix (direct/mule/relay); probe ship-LAN client isolation while aboard | 🔨 One sailing answered the LAN-isolation question and made the same-LAN transport a field-validated design input (§5.4). The instrumented week — latency, battery, mode mix — is still ahead. Meanwhile the family runs CruiseMesh as its daily messenger at home, incl. organic multi-hop deliveries |
| 6 | Media (per §8) | — | after the field test says the foundation holds | 🔨 Inline attachments shipped; chunked media designed |

Milestone 0 was the go/no-go gate: it de-risked the only thing that couldn't be
designed around (iOS background BLE) before any real investment.

## 12. Top risks

1. **iOS background BLE** — the existential one. Backgrounded iOS can still scan for
   a specific service UUID and accept connections, but slowly, and iOS may kill the
   app anyway. Mitigations: state restoration, both-role operation so Android
   centrals can wake iOS peripherals, and UX honesty ("open the app when you sit
   down"). If backgrounded-iPhone↔backgrounded-iPhone sync degrades on some
   device generation, the app survives — that pair just syncs on foreground —
   but expectations must be set.
2. **Steel ship RF** — retired as a risk, priced in as a design input (§3):
   the design leans on the ship's own Wi-Fi (§5.4), mule, and relay — not BLE
   range. Field results (Norwegian Jade) confirmed it: the LAN dominated
   where available and BLE covered the gaps. What remains is per-ship
   variance in LAN client isolation, which the app probes and routes around
   automatically.
3. **Store review** — distribution is no longer hypothetical: both store
   listings are complete, Play closed testing is underway, and TestFlight
   distributes to the beta group. The remaining risk is review friction
   (Apple poking at BLE background modes; Play's first-review pass), managed
   with reviewer notes explaining the no-account model and the two-device
   nature of mesh testing.
4. **Battery** — duty-cycle scanning (e.g. 10 s scan / 50 s idle when on battery,
   aggressive when charging). Budget: <5%/day radio overhead.
5. **Crypto review** — standing gate from SECURITY-DESIGN: a paid independent
   security review before recommending CruiseMesh beyond its stated threat
   model or making any comparative security claims. The design-review
   discipline (libsodium whole, no bespoke constructions, fuzzers in CI,
   adversarial review rounds) is the interim substitute, not the
   replacement. Bridgefy shipped first and got dissected twice.

## 13. Open questions (deliberately deferred)

- Multi-device sync. (Identity backup shipped: a passphrase-encrypted local
  `.cmbak` export/restore of identity + history on both platforms. What
  remains open is two live devices sharing one identity.)
- Message history sync for a group member who joins late.
- Ratchet / post-quantum upgrade timing (envelope `version` byte reserves the path).
- Relay federation.
- A broadcast channel scoped to one Shore Pass (§6.6). The public,
  anyone-with-the-app version is not deferred, it is dropped.

---

## 14. UI / UX

The model is **Signal**: the home screen is the conversation list, because opening
a conversation is the only thing a user launches this app to do. Identity,
friending, and mesh plumbing all live behind it. Material 3 components on Android,
SwiftUI equivalents on iOS — same information architecture on both.

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

One row per chat (1:1 and groups today; a broadcast row would slot in the same
way if the pass-scoped variant is ever built, §14.6), sorted by
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
granted. Honest copy matters here (§12): the pill is where "open the app when you
sit down" expectations get set.

### 14.4 Profile & settings (top-left avatar, like Signal)

Absorbs the old identity screen: editable display name, local/shared profile
photo picker, my QR friend card, UserID + fingerprint words (with the "read
these aloud to verify" hint), mesh on/off, the Shore Pass status indicator
(quiet glyph states; tap for plain-language detail), backup/restore, and the
build version. Nothing here is daily-use, which is exactly why it lives
behind the avatar.

### 14.5 Chat screen

Push-based message list with day separators, the §7.1 lamport-gap indicator
("some messages may still be in transit" — subtle, not an error), a tick-state
legend on tap, and a bubble palette coherent with the avatar hue.

### 14.6 Later, designed for now

- **Groups (M4)**: "New group" action alongside the friends list under the FAB;
  group rows use the same bubble with a group glyph; per-member receipt detail on
  tap in-chat (§7.2).
- **Broadcast**: a clearly labeled row pinned at the bottom of home, visually
  distinct, collapsed by default. Held in reserve for the pass-scoped variant
  in §6.6; the public-to-anyone version it was originally drawn for is
  dropped.

Both slot into the §14.2 list without re-architecting the home screen.

---

## References

- bitchat — BLE mesh design reference: [github.com/permissionlesstech/bitchat](https://github.com/jackjackbits/bitchat) · [overview](https://www.techtarget.com/whatis/feature/What-is-Bitchat) · [Wikipedia](https://en.wikipedia.org/wiki/BitChat)
- Briar (no iOS, threat model contrast): [briarproject.org](https://briarproject.org/) · [iOS issue #445](https://code.briarproject.org/briar/briar/-/issues/445)
- Berty / Wesh protocol (transport lessons): [berty.tech/docs/protocol](https://berty.tech/docs/protocol/) · [github.com/berty/berty](https://github.com/berty/berty)
- Bridgefy cryptanalysis (what not to do): [Breaking Bridgefy, again (USENIX '22)](https://www.usenix.org/conference/usenixsecurity22/presentation/albrecht)
- Meshtastic (hardware alternative that works on ships): [beginner's guide](https://www.elecrow.com/blog/texting-without-cell-service-an-absolute-beginners-guide-to-meshtastic.html)
- Cruise connectivity / RF reality: [Seafy: cruise ship connectivity](https://seafy.com/en/blog/tech-wifi/cruise-ship-connectivity-explained-what-passengers-need-to-k)
