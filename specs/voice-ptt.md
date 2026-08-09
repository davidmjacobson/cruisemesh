# Voice over WiFi: recommendation and spec

Status: proposal, 2026-08-05. Grounded against `core/src/content.rs`, `limits.rs`,
`lan_session.rs`, `protocol.rs`, `relayd/src/lib.rs`, and both shells' manifests.

## Recommendation

**Do not build real-time voice calls. Do build push-to-talk as asynchronous
voice bursts on the existing attachment pipeline, with an optional later
live-PTT fast path over LAN.**

Three-quarters of PTT already exists in the codebase: `CoreAttachmentPayload`
supports `AttachmentMediaType::Audio` with `duration_ms` and a 180 KiB blob cap
(`content.rs:6-22`), `KIND_ATTACHMENT_MANIFEST/CHUNK` are allocated
(`protocol.rs:279-282`), `RECORD_AUDIO` is in the Android manifest, iOS has
`VoiceRecorder.swift` and the mic usage string. PTT is a UX layer plus a codec
decision on top of shipped infrastructure. Calls are a different product.

### Technical reasons against calls

- **The architecture is store-and-forward; calls are circuit-switched.** Every
  delivery guarantee CruiseMesh has (DTN carry, relay mailboxes, digest-proof
  acks, BLE fragmentation at 512 B ATT values) is built for envelopes that can
  wait. A call cannot wait. Calls would need a parallel real-time stack — jitter
  buffers, echo cancellation, RTP-equivalent transport, call signaling, NAT/AP
  traversal — that shares almost nothing with the existing core.
- **Ship WiFi is actively hostile to peer-to-peer real-time.** AP/client
  isolation, captive portals, and mDNS suppression are the norm; LAN discovery
  reliability was a multi-PR battle (#204/#208/#210/#212/#213) for *messaging*,
  which tolerates seconds of outage. A call drops audibly on every one of those
  hiccups. The only reliable call path would be via relayd — see cost below.
- **BLE cannot carry a call and barely carries live PTT.** Realistic GATT
  throughput (~5–15 KB/s with 512 B fragments) is marginal for even 12 kbps
  Opus once mesh chatter shares the link, and the A2DP coexistence problem
  (earbuds stutter; `BluetoothAudioBackoff.swift` exists for a reason) gets
  strictly worse with continuous audio.
- **Relay-carried calls burn the family's own budget.** relayd buckets are
  64 MiB/min per family with a 256 MiB storage quota. A duplex call is
  ~350 KB/min/leg — affordable in bytes, but it converts relayd from a mailbox
  into a media server: persistent per-call connections (WS cap is 16 per
  token, 4 KiB max inbound WS message — sized for signaling, not audio),
  server egress paid by us, and a new abuse surface.

### Practical reasons against calls

- **Real-time audio cannot be verified in CI.** CallKit, AVAudioEngine
  full-duplex, and the `voip` background mode are only exercisable on hardware,
  and real-time audio is the worst category of feature to ship untested. Async
  PTT playback/record is the same `VoiceRecorder` surface already in tree.
- **Calls need new sensitive platform declarations.** They mean
  `foregroundServiceType="microphone"` on Android (a sensitive-FGS declaration
  with its own review) and `voip`/CallKit on iOS. PTT recording only in a
  foreground activity needs **no new manifest surface on either platform.**
- **Automated verification stops at the envelope.** The two-phone rig can
  verify an async voice envelope end-to-end with a scripted assertion (blob
  digest + duration). It cannot verify echo cancellation, which needs two
  people in two places at the same time.

### Product reasons

- The family-on-a-cruise job is "come to dinner," "we're at the pool deck,"
  "call me on the cabin phone" — bursts, not conversations. Ships already have
  cabin phones for conversations; nobody has a walkie-talkie. PTT *is* the
  walkie-talkie, and it degrades gracefully: on dead WiFi it arrives 40 seconds
  later via BLE or carry instead of failing.
- A call that connects and then breaks up teaches the family the app is
  unreliable — trust damage that bleeds onto messaging. A voice burst that
  arrives late is still a delivered voice burst.
- Product bar: hold-to-talk is obvious to a 10-year-old and a grandparent.
  Call UX (ringing, missed calls, busy states, audio routing pickers) is a
  large surface with no simple version.

---

## Spec: Phase 1 — PTT voice bursts (recommended)

One-line: a hold-to-talk button that records a short Opus clip and sends it as
a normal attachment envelope, with walkie-talkie receive semantics for
capable, opted-in recipients.

### 1. Codec and blob format

- **Opus, 16 kHz mono, VBR ~16 kbps target, 20 ms frames, complexity 5.**
  Voice-optimized (`OPUS_APPLICATION_VOIP`). ~2 KB/s → a 60 s burst is
  ~120 KB, inside `ATTACHMENT_MAX_BLOB_BYTES` (180 KiB) with headroom.
- **Encode/decode in the Rust core** (`opus` crate wrapping libopus; builds
  clean for NDK and the iOS CI toolchain). Core-first: shells capture/play PCM
  (16-bit, 16 kHz) and call `core_ptt_encode(pcm) -> blob` /
  `core_ptt_decode(blob) -> pcm` via UniFFI. This gives one bitstream
  implementation, Rust-unit-testable (round-trip, truncation, garbage input,
  duration honesty), and no dependence on per-device MediaCodec/AudioConverter
  codec availability.
- Blob wire format inside `CoreAttachmentPayload.blob`: 1-byte version, then
  repeated `u16-le packet length + Opus packet`. `mime_type = "audio/x-cruisemesh-ptt"`.
  `duration_ms` computed by core from frame count, never trusted from the peer
  beyond a sanity clamp (decode stops at 90 s regardless of header).
- **Hard caps:** 60 s record limit in UX; core rejects encode >60 s and decode
  >90 s. Clip, don't fail: hitting the cap while talking sends what was said.

### 2. Wire and protocol

- **No new frame type. No new message kind.** A PTT burst is a
  `KIND_ATTACHMENT_MANIFEST/CHUNK` audio attachment, sealed and routed exactly
  like a photo. It inherits BLE fragmentation, LAN Noise sessions, relay
  mailboxes, DTN carry, group fan-out (`seal_group_message` once +
  `core_group_fanout_rows`), acks, and both privacy invariants untouched.
- **PTT flag:** add an optional `ptt: bool` field to `CoreAttachmentPayload`
  under a bumped `ATTACHMENT_WIRE_VERSION`, decoder tolerant of both versions.
  Old clients render the burst as a regular voice memo — correct degradation,
  nothing breaks. (Attachments are internally versioned; this is not the
  legacy-HELLO trailing-field trap.)
- **Capability bit:** one new bit in `core_own_capabilities()` riding HELLO2
  (0x06): "understands PTT semantics." Used only for sender-side UX (show
  "voice memo" vs "PTT" affordance per contact), never for gating delivery.

### 3. UX

- **Send:** mic button beside the composer. Hold to record (haptic +
  elapsed-time pip + live level meter), release to send, slide-up to lock
  hands-free, slide-left to cancel. Under ~700 ms of audio = discard (pocket
  press). Works identically in 1:1 and group chats.
- **Receive:** bubble with play button, duration, and a scrub-free progress
  bar. Tap to play; playing queues consecutive unheard bursts from the same
  chat in order (walkie-talkie catch-up).
- **Auto-play (the walkie-talkie feel):** per-chat toggle, default OFF,
  surfaced the first time a PTT burst arrives ("Play voice messages from Mom
  automatically?"). Auto-play only when: app foregrounded, that chat open or
  device explicitly in "walkie mode," media volume audible, and not during an
  active A2DP-coexistence backoff. Never auto-play from a notification, never
  on the lock screen.
- **Routing:** loudspeaker by default; earpiece when the proximity sensor is
  covered (Android) / receiver route (iOS). Respect connected
  headphones/earbuds; if the A2DP mitigation banner is active, prefer the
  phone speaker rather than fighting the shared radio.
- Strings in `strings.xml`/`Localizable.xcstrings`; sentence case; no jargon
  ("Voice message," "Hold to talk").

### 4. Platform notes

- **Android:** `AudioRecord` (VOICE_COMMUNICATION source for AGC/NS) →
  core encode. Playback via `AudioTrack` from core-decoded PCM. Recording only
  while the activity is foregrounded — **no FGS type change, no new
  permissions, no Play declaration delta.** `MeshService` untouched.
- **iOS:** extend `VoiceRecorder.swift` (AVAudioEngine tap → PCM → core).
  `AVAudioSession` category `.playAndRecord`, mode `.spokenAudio`, activated
  only around record/play and deactivated after, so it never contends with the
  BLE mesh background modes. No `UIBackgroundModes` changes. Ships CI-only:
  the risk is UX polish, not correctness — correctness lives in core tests.
- **Both platforms in the same PR** per standing rule; core + Android
  verifiable locally, iOS compiled by ios.yml.

### 5. Transport behavior and budgets

- LAN/relay: a 120 KB envelope is trivial (LAN frame ceiling 1 MiB, relay cap
  512 KiB, family bucket 64 MiB/min ≈ 500+ bursts/min).
- BLE-only worst case: ~120 KB across ~240 fragments ≈ 15–60 s delivery.
  Acceptable for async; show the normal sending/sent ticks, no special casing.
- DTN carry: bursts are carried like any attachment. Consider (follow-up, not
  v1) capping carried PTT age harder than text — a 2-day-old "where are you"
  is noise — via the existing expiry field at authoring time (e.g. 6 h expiry
  for PTT vs default), which old peers already honor.

### 6. Testing

- **Core (bulk of the assurance):** Opus round-trip PCM fidelity bounds;
  duration honesty; cap enforcement; wire-version up/down-grade decode;
  fuzz target on `core_ptt_decode` alongside the existing fuzz gates.
- **Android unit:** composer state machine (hold/lock/cancel/short-press) as a
  plain class, no Android imports.
- **Two-phone rig:** extend `tools/two_phone_ble_smoke.sh` with a PTT leg —
  send burst on A, assert on B via receipt watermark + decoded duration; run
  the LAN and BLE-only variants. This is the release gate.
- **Manual:** earbud coexistence check (the A2DP stutter re-verify is already
  owed), group burst to 3+ devices, iPhone via TestFlight.

### 7. Rollout

1. Core codec + payload flag + capability bit, behind nothing (inert without UI).
2. Android UI + rig test.
3. iOS UI same PR, CI-compiled, verified via TestFlight build.
4. No server changes. No listing changes (mic permission already declared and
   described on both stores).

---

## Phase 2 (optional, explicitly deferred): live PTT over LAN

Only worth doing if families ask for real-time feel. Design constraints so it
stays cheap if built:

- **Half-duplex only, LAN only, direct Noise session only.** New frame type
  `0x07 VOICE_BURST_STREAM`: stream the same Opus 20 ms packets over the
  existing per-peer TCP Noise session as they're encoded, with a 300–500 ms
  jitter buffer at the receiver. On session loss or no LAN path, the burst
  transparently completes as a Phase 1 envelope — same bytes, so the fallback
  is a re-frame, not a re-encode. Never streamed over relay (cost) or BLE
  (bandwidth); those get the async envelope.
- Receiver plays live only under the same auto-play consent; otherwise the
  stream is just early delivery of the envelope.
- This is additive UX latency polish. It does not change the recommendation:
  ship Phase 1, measure whether anyone notices the latency, and only then
  decide.

## Explicitly rejected

- **Full-duplex calls (1:1 or group), WebRTC, CallKit/ConnectionService,
  relay-mediated audio.** Reasons in the recommendation. If this ever becomes
  a real demand, it should be scoped as its own product (likely
  WebRTC-over-relayd-TURN with per-family metering) rather than grafted onto
  the mesh core — and it should wait for a hardware-testable pipeline.

---

## As built (Phase 1)

The proposal above is unchanged from 2026-08-05. This section records where the
implementation deliberately departed from it, so the next reader does not treat
a considered decision as an oversight.

- **Codec: AAC-LC in MPEG-4, not Opus.** The proposal put an `opus`-crate codec
  in the Rust core. Platform-native Opus is not an option — Android's encoder
  writes Ogg, iOS's writes CAF, and neither platform's player reads the other's
  container — so honouring it would have meant vendoring libopus into the NDK
  and iOS CI toolchains and replacing both shells' capture and playback with raw
  PCM pipelines. That is a large change with no device coverage behind it, and
  it would make every voice message unplayable on already-shipped clients.
  AAC-LC is what both shells already write and both already read. What the
  proposal's numbers were really buying — ~2 KB/s so a full-length burst fits
  180 KiB with headroom — is honoured: 20 kbps mono at 16 kHz is 2.5 KB/s, and a
  60 s message is ~158 KB of the 180 KiB cap. Before this, both shells recorded
  at 32 kbps, so anything past ~46 s was rejected as too large *after* the user
  had spoken it.
- **The duration bound is derived, not declared.** `voice_capture_plan()` in
  `core/src/voice.rs` computes it from the blob cap, the container overhead and
  a headroom reserve, and both shells read it. There are no per-platform copies
  of the bitrate, the bound, the minimum hold, or the slide thresholds.
- **The clock is not the only bound.** A duration derived from a bitrate only
  holds if the encoder honours that bitrate, and AAC encoders vary in how low a
  bitrate they accept at 16 kHz mono. So both shells weigh the file the encoder
  is actually writing on every 100 ms tick and stop on whichever bound arrives
  first (`voice_capture_bytes`). Being byte-bound costs the user seconds; being
  unbound costs them the whole recording, after they have already spoken it.
- **A hold is not the only way in.** `voice_capture_start_hands_free` enters the
  same locked state directly, because neither a screen reader nor a switch can
  express "press and keep pressing". Android reaches it through a semantics
  action on the mic; iOS through the Start/Stop sheet.
- **A locked recording is cancelled when the app leaves the foreground.**
  Hands-free is the first state where recording outlives the finger, and neither
  platform lets a foreground-only app keep a live microphone in the background.
  Android watches `ON_STOP`; iOS watches `scenePhase == .background` and
  `AVAudioSession.interruptionNotification`. The user is told the recording
  stopped rather than being sent a minute of silence.
- **No `ptt` payload flag and no HELLO2 capability bit.** Both were proposed for
  sender-side affordance only ("show PTT vs voice-memo per contact"). Nothing on
  the wire changed: a voice message is an ordinary audio attachment.
- **Android audio source stays `MIC`.** The proposal suggested
  `VOICE_COMMUNICATION` for its gain control. That source asks for a
  communication-tuned uplink, which on some devices moves capture onto a
  headset's hands-free profile — the same radio the mesh runs on. Nothing about
  that is verifiable off-device, so the field-proven source stayed.
- **Deferred, not rejected:** auto-play of arriving messages and its consent
  surface, queuing consecutive unheard messages, proximity-sensor earpiece
  routing, the live level meter, a shorter carry expiry for voice, and the
  two-phone rig's voice leg. None of them changes anything shipped here.
