# Cellular gateway ("balcony uplink")

Status: draft (2026-07-18).

## 1. Problem

Field observation: on many ships a phone held near a balcony or open deck
picks up shore/coastal 5G while remaining associated to the ship Wi-Fi. That
phone is simultaneously on the family's LAN (same-LAN transport,
[`specs/same-lan-transport.md`](same-lan-transport.md)) and on the internet —
the perfect family uplink. Nobody bought a Wi-Fi package; one phone parked on
the balcony can still move the whole family's mail through the relay.

The obvious generalization — run a SOCKS/HTTP proxy on the balcony phone and
tunnel arbitrary internet for other devices — is **not** what CruiseMesh
should build (§6). The CruiseMesh-shaped version is narrower and mostly
already exists: sealed envelopes flow phone→gateway over LAN/BLE, and
gateway→relayd over cellular. This spec closes the remaining gaps so that
path is deliberate, observable, and controllable instead of accidental.

The decisive technical requirement (confirmed by outside research on
LAN-proxy apps): *listen on Wi-Fi, but bind outbound sockets to the cellular
`Network`* — per-socket binding via `Network.socketFactory`, never
`bindProcessToNetwork()`, which would drag the LAN listener onto cellular
too. This is already CruiseMesh's implemented approach on Android (§2).

## 2. What already works

The gateway loop is ~90% built on Android; this section is the inventory so
the gaps in §3 are legible.

- **Per-socket network pinning.** `MeshService` holds a
  `requestNetwork(NET_CAPABILITY_INTERNET)` and pins relay HTTP traffic to
  the granted network once it reports `VALIDATED`
  (`MeshService.relayBindNetwork`, used by every `RelayClient` call). When
  the associated-but-dead ship Wi-Fi is still the system default, relay
  sync already rides a validated cellular network while the LAN transport
  keeps using Wi-Fi. DNS resolves through the pinned network
  (`Network.openConnection`).
- **Proxy-hint fetching.** An internet-connected phone polls the relay for
  mail addressed to *every contact*, not just itself
  (`relayProxyHints`), and carries fetched envelopes to the recipient over
  BLE/LAN (`carryRelayEnvelope`). The relay push WebSocket subscribes with
  the same proxy hints, so inbound mail for a nearby internet-less family
  member wakes the gateway immediately.
- **Eager outbound flush.** A family envelope arriving over BLE/LAN
  triggers an immediate relay sync pass ("family carry queued" in
  `MeshService`), so the gateway uploads a cabin-mate's message seconds
  after receiving it, not on the next 60s poll.
- **LAN transport.** Envelopes move between family phones across the ship
  LAN with Noise authentication, independent of internet access.

Net: put one phone with working cellular on the balcony and every family
phone reachable over the ship LAN already sends and receives through it.
What follows makes that reliable instead of lucky.

## 3. Gaps

- **G1 — cellular is accepted, never requested.** The existing
  `requestNetwork(INTERNET)` lets the framework *offer* cellular when Wi-Fi
  fails validation, but whether mobile data comes up at all — and whether it
  stays up with a marginal balcony signal while Wi-Fi is associated — is
  OEM- and framework-dependent. Nothing in the app expresses "I want the
  cellular path held up."
- **G2 — relay push WebSocket is unpinned.** `RelayPushClient` deliberately
  skips network pinning; when the default network is the dead ship Wi-Fi
  the WS connect fails and delivery degrades to the 60s poll. That punt was
  priced as "rare edge case"; gateway mode makes it the *defining* case —
  the gateway loses push latency exactly when it is the family's only
  uplink.
- **G3 — no metered/roaming policy.** Relay sync will silently consume
  roaming data the moment cellular validates, or silently do nothing if the
  user has mobile data disabled. Neither failure is visible.
- **G4 — no visibility or control.** The user cannot see that their phone is
  acting as the family uplink, over which network, or turn the cellular
  path on/off independently of mobile data as a whole.
- **G5 — iOS parity unknown.** iOS relay traffic rides whatever path the
  system prefers; no pinning exists and the Android failure mode has not
  been observed there. Whether iOS keeps cellular usable while associated
  to a no-internet Wi-Fi on a real ship is an open field question.

## 4. Design

### 4.1 "Use mobile data" gateway toggle (G1, G3, G4)

A new setting, **Use mobile data for message sync**, default **on** — the
platform's promise is "do whatever possible to get messages through," and
the traffic is tiny (§4.4). A first-sync-over-cellular-while-roaming
one-time notice explains the data use and points at the toggle.

While the toggle is on and the mesh service is running with a relay
configured, `MeshService` files a **second** network request:

```
requestNetwork(TRANSPORT_CELLULAR + NET_CAPABILITY_INTERNET)
```

alongside the existing generic INTERNET request. An explicit
`TRANSPORT_CELLULAR` request is the Android-sanctioned way to hold mobile
data up while Wi-Fi is connected; without it the framework may never bring
cellular up behind an associated Wi-Fi. Selection order for
`relayBindNetwork` when multiple networks are validated:

1. validated non-cellular (paid ship Wi-Fi, port Wi-Fi) — free wins;
2. validated cellular from either request.

Toggle off ⇒ release the cellular request and never bind relay traffic to a
`TRANSPORT_CELLULAR` network, even one the framework offers unprompted.
Everything else (BLE, LAN, relay over Wi-Fi) is unaffected.

Battery note: holding a cellular request keeps the modem attached where
signal is marginal. The request is held only while the mesh foreground
service runs, which is already the app's battery envelope; measured cost is
a §7 gate, and the existing duty-cycle philosophy (DESIGN.md §12.4) applies
if it proves expensive.

### 4.2 Pin the relay push WebSocket (G2)

Revisit `RelayPushClient`'s documented punt: build the OkHttp client
per-connect with `socketFactory(network.socketFactory)` and a `Dns`
implementation backed by `network.getAllByName`, using the same
`relayBindTarget()` the HTTP path uses (null ⇒ default network, as today).
The doorbell contract is unchanged — poll remains the correctness
authority — but the gateway keeps push latency in the exact scenario this
spec exists for. Reconnect/backoff logic is untouched; a network change
simply causes the next reconnect to bind to the new target.

### 4.3 Status surface (G4)

- Mesh status pill: `Syncing via relay · mobile data` when
  `relayBindNetwork` is cellular (network class is already computed —
  `networkLabel`).
- Profile/diagnostics screen: which network relay sync last used, last
  success time, and bytes moved over cellular this cruise (a resettable
  counter — cheap honesty about §4.4's claims).
- No change to the foreground notification beyond the pill text; the
  service already exists.

### 4.4 Data budget

No hard cap in v1; instead, size the traffic honestly and gate media:

- Text and receipt envelopes are padded to 256-byte buckets (DESIGN.md
  §6.3); with header and transport overhead a message is well under 1 KiB.
  A chatty family day is tens of KiB — noise even on roaming rates.
- Profile photos are ≤ 24 KiB full-size (DESIGN.md §6.2.1) — allowed.
- Future chat attachments (DESIGN.md §8, kinds 16/17) are the only thing
  that could hurt. When they ship, chunk transfer over a cellular-bound
  relay connection gets its own sub-toggle (**Send media over mobile
  data**, default off). The reserved decision is recorded here so media
  work doesn't rediscover it.

### 4.5 Gateway visibility over the LAN (optional, phase 2)

Delivery needs no advertisement — sync is symmetric and the gateway loop in
§2 works without any peer knowing who has internet. The only value is UX on
the *internet-less* phones ("connected via Dave's phone") and smarter status
copy ("your message will reach Grandma via the balcony phone"). If built: a
one-byte capability flag ("relay reachable") on the existing authenticated
link-control channel (post-HELLO, like `LAN_ENDPOINT`), shared only with
accepted contacts over Noise-encrypted links, never advertised in DNS-SD
TXT or BLE advertisements where strangers on the ship LAN could enumerate
who has paid internet. Deferred until the field test (§7) shows the core
loop working.

### 4.6 iOS (G5)

No speculative pinning. iOS's path selection already prefers a working
cellular path over a non-validating Wi-Fi in common cases, the failure mode
has not been observed there, and the platform offers no direct analog of
`requestNetwork`. Two facts shape what's worth building:

- The iOS relay push socket only lives in the foreground anyway
  (`RelayPushClient.swift` class doc), so the balcony-gateway phone is
  realistically a *foregrounded* phone — which is also when path selection
  behaves best.
- If the field test shows iOS refusing to use cellular behind ship Wi-Fi,
  the remedy is scoped in advance: relay networking moves to
  `NWConnection` with `requiredInterfaceType = .cellular` as a fallback
  attempt after a default-path failure, honoring
  `allowsExpensiveNetworkAccess` and the §4.1 toggle.

The §4.1 toggle and §4.3 status copy ship on iOS regardless, so behavior
and controls stay symmetric even where the plumbing differs.

## 5. Privacy and security

- Phase 1 adds no new wire surface: no new frames, no new relay API, no new
  discovery data. It changes only which local network existing TLS relay
  traffic binds to.
- The cellular carrier observes TLS to the family relay host — already true
  whenever cellular was used (in port, at home). Envelope contents and
  receipts remain sealed end-to-end; the relay's knowledge is unchanged
  (DESIGN.md §6.4).
- The phase-2 capability flag (§4.5), if built, discloses "this phone has
  internet" — to authenticated accepted contacts only, over the Noise
  channel. It must never appear in unauthenticated advertisements.
- Unlike a generic LAN proxy, there is no open port offering internet to
  strangers: the only things a gateway will do for another device are the
  authenticated CruiseMesh sync operations it already does, so there is no
  "someone on deck 9 found my proxy port and streamed video over my
  roaming plan" failure mode.

## 6. Non-goal: generic internet sharing

The researched proxy setup (SOCKS5/HTTP proxy on the gateway phone, other
devices' browsers or VPN-clients pointed at it) is explicitly out of scope:

- **Mission.** CruiseMesh gets *messages* through. General tethering is a
  different product with none of our E2EE/DTN machinery applicable.
- **Exposure.** An open proxy port on a hostile ship LAN invites strangers
  to burn the user's roaming data; doing it safely means building
  authentication and rate-limiting for a feature we don't want.
- **Policy.** Carrier terms commonly restrict tethering/traffic sharing,
  and app-store review for a proxy-server app is its own project.
- **Redundancy.** For the traffic we care about, §2's envelope path already
  shares the connection — with authentication, encryption, dedupe, and
  store-and-forward that a raw proxy lacks.

Users who want general internet sharing can run a dedicated proxy app next
to CruiseMesh; nothing here conflicts with that.

## 7. Validation gates

Before declaring the feature done (and before building §4.5 or §4.6's
fallback):

1. Bench: Android phone associated to a no-internet Wi-Fi AP with mobile
   data on — toggle on holds cellular up and relay sync binds to it; toggle
   off provably never touches cellular.
2. Push WS reconnects onto the pinned network and delivery latency stays
   push-scale (seconds, not 60s poll-scale) in the dead-Wi-Fi case.
3. Flapping cellular (balcony edge-of-coverage): bind target follows
   validation transitions without wedging sync; no duplicate rendering
   (msg_id dedupe holds — expected free, verify anyway).
4. OEM spread: at least Pixel and Samsung builds honor the explicit
   cellular request behind associated Wi-Fi.
5. Data counter vs. reality: a simulated family-day of traffic over the
   cellular path measures within sane bounds of §4.4's estimates.
6. Battery: overnight hold of the cellular request at weak signal, versus
   toggle off, stays inside the existing radio budget (DESIGN.md §12.4).
7. Ship field test (DESIGN.md §11 M5): balcony phone as sole uplink; log
   delivery mode mix and latency for on-ship↔shore messages; observe
   whether iOS uses cellular behind ship Wi-Fi unaided.
