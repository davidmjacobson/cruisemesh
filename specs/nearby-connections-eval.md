# T3 — Google Nearby Connections evaluation

Status: spike (spec-only, no product code). Timeboxed deliverable. Recommendation
up front, evidence below.

## Recommendation: **No — do not replace the BLE GATT mesh with Nearby Connections.** Keep our transport; steal one idea (the Wi-Fi upgrade path).

## Why we asked

Nearby Connections promises automatic transport selection (BLE → BT Classic →
Wi-Fi Direct/hotspot) and handles discovery/connection/upgrade for you. On
paper that removes a lot of the BLE GATT plumbing we maintain. The five
questions we set out to answer:

1. Does it deliver while the app is **backgrounded / screen off**?
2. Is it **cross-platform** (Android ↔ iOS) on the same protocol?
3. Does taking over the radios re-open the **A2DP audio coexistence** problem we
   already tuned around?
4. Does its connection model fit our **many-peers mesh + muling** shape, or is it
   built for 1:1/small-N sessions?
5. What would migration cost, and what's the **blast radius** on the parts that
   work today?

## Findings

### 1. Background operation — the likely killer
Our delivery guarantee depends on the mesh working while the phone is in a
pocket: the Android foreground service keeps BLE advertise + GATT server/central
running with the screen off, which is how messages hop mule→mule unattended.
Nearby Connections is **foreground-oriented** — discovery/advertising is
expected to run with the app in the foreground; background behaviour is
restricted and unreliable across OEMs and Android versions. Losing unattended
background delivery would gut the product's core promise. **This alone is
disqualifying** unless proven otherwise on our target devices, and the burden of
proof is high.

### 2. Cross-platform — not on one protocol
Nearby Connections is **Android-only**. iOS has no compatible peer. Our BLE GATT
mesh is a custom protocol we run identically on both platforms (see
`ios/CruiseMesh/Mesh/BleTransport.swift` vs Android `BleCentral`/`BlePeripheral`).
Adopting Nearby would fork Android onto an incompatible transport, so an
Android↔iPhone pair — a primary use case — could not mesh over it. Non-starter
for a two-platform app.

### 3. A2DP coexistence — re-opened
We spent real effort tuning BLE radio settings to coexist with Bluetooth audio
(A2DP) so earbuds don't stutter while the mesh runs; that work lives in our
advertising/scan parameters and the A2DP backoff. Nearby manages the radios
itself and would likely escalate to BT Classic / Wi-Fi Direct, which is exactly
the kind of aggressive radio use that fights A2DP. We'd lose the control that
made our mitigation possible.

### 4. Connection model — wrong shape
Nearby's model is oriented around explicit connection requests between a modest
number of endpoints. Our mesh is many transient peers, opportunistic muling of
sealed foreign envelopes, and digest-driven convergence — we don't want a
handshake/accept per peer, and we specifically want to carry envelopes for
strangers. Nearby's abstraction hides the very layer (per-link frames, seen-set,
carry queue) our DTN design is built on.

### 5. Migration cost / blast radius
Replacing the transport touches `MeshRouter`, both platforms' BLE layers, the
A2DP coexistence tuning, and the discovery/HELLO/DIGEST plumbing — the most
load-bearing, hardest-won code in the app — to gain an Android-only,
foreground-only transport. The risk/reward is badly inverted.

## Idea worth stealing: the Wi-Fi upgrade path

Nearby's one genuinely nice trick is **starting on BLE for discovery, then
upgrading a live link to a high-bandwidth Wi-Fi path** for bulk transfer. We
already have the pieces to do this ourselves without adopting Nearby:

- BLE is our discovery + control channel.
- We already have a same-LAN Noise-XX TCP transport (`same-lan-transport.md`)
  and LAN endpoint hints exchanged over BLE (`KIND_LAN_ENDPOINT_HINT`).

The steal: when two meshing phones discover they're on the **same Wi-Fi**
(already detected for the LAN transport), prefer the LAN path for large frames
(we do a version of this in `core_transport_send_plan`), and consider a
**BLE-negotiated ad-hoc Wi-Fi** path (Wi-Fi Direct / local-only hotspot) for
peers **not** on a shared LAN — discovery/negotiation over our existing BLE
control channel, bulk over Wi-Fi. That keeps our cross-platform protocol and
background model while capturing Nearby's bandwidth win. File as a **future
transport spike** if LAN field data (L3) shows BLE bandwidth is the bottleneck
for media.

## Disposition

- T3: **closed as won't-adopt**, cite this doc.
- Follow-up (optional, data-driven): "BLE-negotiated Wi-Fi bulk path" spike,
  gated on L3 media-throughput findings.
