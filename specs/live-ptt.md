# Live push-to-talk: design specification

Status: Proposed (rev 1)
Platforms: Android and iOS (core-shared policy)
Scope: real-time walkie-talkie voice, distinct from the shipped async voice
messages

> **Provenance.** This rev-1 spec synthesizes three independent design passes
> made on 2026-08-09 — one mapping the code-level constraints, one proposing
> the architecture, one doing the transport feasibility arithmetic — each then
> adjudicated against source before integration. Where they diverged is
> recorded in the appendix.

## Outcome

Hold a button and your voice reaches nearby family in near-real-time — a
walkie-talkie — when the network allows it, and becomes an ordinary voice
message when it doesn't. It must never lose the utterance, never weaken the
mesh's safety model, and never pause store-and-forward delivery.

## The one rule

**Live PTT is a new ephemeral fast path layered over the existing authenticated
LAN Noise session, and nothing else.** It does not reuse the message pipeline;
the message pipeline is its safety net.

- Live audio travels ONLY to directly-connected, authenticated LAN peers.
- Live frames are NEVER sealed as envelopes, gossiped, carried by a mule,
  deposited at the relay, or forwarded by a third phone.
- BLE and relay carry only the ordinary async voice-message fallback.
- Recording ALWAYS produces a durable fallback artifact while streaming; if
  live can't start, breaks up, or can't prove complete playback, the utterance
  is delivered as a normal `audio/mp4` voice message.
- Store-and-forward never pauses for audio. Under resource contention, LIVE
  yields and falls back; durable mesh work is never suspended or discarded.

Everything below follows from that rule and from what the code actually permits.

## Why not the existing pipeline (code-grounded constraints)

Gemini's read of the current transport/audio code found these hard blockers to
routing real-time audio through the normal path — each is why live PTT needs a
separate lane:

- **Envelope + crypto overhead is 59+ bytes** (`ENVELOPE_FRAME_OVERHEAD` 34 +
  MAC 16 + Noise record 9) on a 40-byte 20 ms Opus frame — >140% tax if each
  frame were sealed. Live frames use a lightweight header, not envelopes.
- **The gossip handshake** (HELLO→DIGEST→OFFER→WANT→DATA) is 200 ms–2 s before
  data flows, with a 3–5 min re-digest cadence. Real-time can't negotiate
  availability per frame.
- **The DTN carry/SQLite path** persists and re-offers; ephemeral audio (useless
  after ~500 ms) would thrash it. Live audio is never persisted as mesh state.
- **The BLE fragment reassembler drops the ENTIRE frame if one fragment is
  lost** (`core/src/framing.rs`). Real-time audio needs per-frame independence and loss
  tolerance, not all-or-nothing.
- Transport ceilings: BLE 1 in-flight write, 8-peer cap, ~300 ms connect + 5 s
  reconnect backoff; LAN fast once up but gated by Noise-XX setup, a 15 s
  election fallback, and ship-AP hostility.

## Feasibility verdict (the transport math)

Grok's arithmetic (16 kbit/s Opus, 20 ms frames, ≤500 ms mouth-to-ear target):

| Transport | Verdict | Why |
|---|---|---|
| **LAN (Wi-Fi TCP)** | **GO** | ~90–150 ms m2e; comfortably under budget. The only issue is *reachability* (AP client-isolation / mDNS suppression), not latency. |
| **BLE, 1 peer** | **MARGINAL** | Fits (~150–350 ms typical) with throughput headroom, but tightens under mesh contention and long connection intervals. |
| **BLE, group fan-out** | **NO-GO past ~2–4 peers** | Independent unicast streams over one shared radio; breaks at N=2 on a contested link, N=5–6 optimistically. |
| **Relay / multi-hop** | **NO-GO** | A mailbox can't do media-grade latency/priority; hop×store blows the budget. Fine for async, not live. |

**Conclusion: LAN-only for v1.** BLE live PTT is deferred (marginal, and the
strict reassembler + 1-in-flight-write make it a project of its own). Relay
live audio is a permanent non-goal.

## Non-goals

- Full-duplex calls (this is half-duplex press-to-talk).
- Live audio over BLE, relay, or any carried/multi-hop path (v1).
- Resuming a live burst after a mid-burst reconnect (v1 finishes as one async
  message rather than splicing around an inaudible gap).
- Any real-time guarantee on an overloaded or client-isolated Wi-Fi network —
  there, it falls back, honestly.
- Pausing or slowing the mesh for audio.

## Capture: dual-encode (the load-bearing decision)

The shipped recorder produces only a finalized AAC/M4A file and cannot emit
20 ms PCM frames. Live PTT therefore needs a **new capture pipeline that
encodes twice, simultaneously**, from one mic tap:

- **Live stream:** Android `AudioRecord` / iOS `AVAudioEngine` input tap →
  16 kHz mono PCM → shared Rust/libopus encoder → 20 ms Opus frames.
- **Fallback artifact:** the same PCM → platform AAC encoder + MP4 muxer,
  building the `audio/mp4` voice message incrementally, under the existing byte
  and 60 s bounds. Finalized on release; **deleted only after live completion is
  proven.**

This is deliberate dual encoding. It is the only way to get low-latency
cross-platform packets while preserving the shipped fallback contract without
losing or re-recording speech.

> **⚠ The real cost of this feature, stated plainly for the go/no-go:** live PTT
> requires **vendoring and hardware-validating libopus on both platforms** — a
> new native dependency the async voice feature deliberately avoided (AAC
> sufficed there). The earlier `voice-messages.md` idea of "stream Opus and
> reframe the same bytes as the fallback" is **not viable**: the shipped
> fallback is AAC/M4A, and old builds can't reliably play a raw-Opus container.
> Hence dual-encode. This is the main implementation-weight decision.

## Live wire protocol (summary; full frame layouts in the design report)

New frame type `0x07 LIVE_PTT` (version + subtype), valid ONLY after an
authenticated LAN Noise handshake; unknown-frame-drop means old clients ignore
it safely. A random `burst_id` scopes one press-to-talk gesture.

- **START** — target, codec params (Opus/16 kHz/mono/20 ms), max duration,
  plus an **Ed25519 signature** over (domain-sep, version, burst_id, target,
  params). Author identity comes from the Noise static-key→contact mapping,
  NOT from the unauthenticated HELLO id. (Noise already encrypts the link;
  signing START keeps the real-time path from becoming a weaker unsigned
  channel.)
- **READY / DECLINED / BUSY / UNSUPPORTED** — the receiver's consent + floor
  response. The sender buffers ≤500 ms of Opus while waiting; **no audio is
  sent before READY** (no playback to a recipient who hasn't consented). No
  READY in ~500 ms → the burst becomes fallback-only.
- **AUDIO** — 1–3 Opus packets per network frame, strictly-increasing
  sequences; duplicates ignored, impossible jumps/oversize/unknown-burst abort
  that burst locally.
- **END** — final sequence + duration + a digest over the ordered packet
  sequence. The receiver sends **COMPLETE only after** every sequence arrived,
  the digest matched, and the audio actually played to an output route without
  material underrun.
- **ABORT** — deliberate slide-to-cancel: discard, delete the fallback, create
  no message. A remote ABORT can only affect the matching active burst from
  that authenticated peer — **no remote frame can cancel local recording,
  delete durable content, clear queues, or stop the mesh.**

## Eligibility and routing

A peer is LIVE-eligible only when ALL hold: an active `CoreTransport::Lan`
route; Noise handshake complete; remote static key maps to an accepted stored
contact; peer advertised `CAP_LIVE_PTT_V1` in HELLO2; transport health good
(no writer backlog, recent probe RTT ≲200 ms); and the receiver returned READY.

A dedicated `live_route_for(user_id)` returns an authenticated LAN route or
**nothing** — it must NOT fail live audio over to BLE the way the ordinary
router may. A mid-burst LAN drop goes to fallback, never to BLE streaming. And
never launch a subnet scan to make the user wait — if no LAN link is already
ready, begin the async voice message immediately.

## Jitter, latency, stale-audio

v1 transport is ordered TCP, so loss shows up as a stall / head-of-line block.
Policy: 3 packets/60 ms per network frame; initial receive buffer ~300 ms;
adaptive 200–500 ms by arrival variance; total queued horizon ~750 ms. On a
stall: ≤120 ms Opus PLC, then fade; if >750 ms behind the head, drop old frames
and mark the burst degraded; if ~1 s with no packet, abort live (a degraded
receiver never sends COMPLETE, so the sender produces the durable fallback).
Realistic target: **400–600 ms typical, <700 ms p95** — walkie-talkie latency,
not telephone latency.

## Fallback (the safety net) and dedup

Produce an async voice message if ANY of: no authenticated LIVE LAN route at
press time; START unanswered by the deadline; DECLINED/BUSY/UNSUPPORTED; LAN
dropped mid-burst; backlog/jitter exceeded the deadline; receiver reported
degraded; END/COMPLETE missing; (group) any required member absent or didn't
complete; capture interrupted without an explicit user cancel. The `burst_id`
correlates a fallback to a possibly-partially-heard live burst so the recipient
isn't double-notified for the same utterance.

## Proposed contract invariants

- **LIVE-01 — lane isolation:** live-PTT frames never enter a sealed envelope,
  the gossip/digest path, a carry queue, the relay, or a multi-hop forward.
- **LIVE-02 — authenticated-only:** live audio is accepted only from a peer
  whose Noise static key maps to an accepted contact; START is signed; the
  unauthenticated HELLO id is never trusted as author.
- **LIVE-03 — never lose the utterance:** every press that captured audio
  results in either proven live playback (COMPLETE) or a durable async voice
  message; the fallback artifact is deleted only after COMPLETE.
- **LIVE-04 — mesh is never paused or corrupted by live:** no live control
  frame can stop the mesh, delete durable content, clear queues, or advance/
  ack anything; under contention live yields.
- **LIVE-05 — consent before playback:** no audio is delivered for playback
  before the receiver's READY.

## UX (family-obvious)

Hold to talk; a clear "live" vs "sending as voice message" indication so the
user knows which happened; slide-to-cancel discards; the recipient hears live
audio only after opting into live playback for that chat (otherwise it arrives
as a voice message). All copy in resources; no protocol jargon. The failure-to-
fallback path is silent-and-graceful, not an error.

## Delivery phases

**Phase 1 — the smallest credible win:** foreground, half-duplex, **1:1**, over
an **already-established** LAN link, both platforms; dual-encode with async
fallback; the full START/READY/AUDIO/END/ABORT protocol; no group, no BLE, no
scan-to-connect. This is the whole feature at minimum viable scope.

**Phase 2 — direct LAN group fan-out:** 1:N over authenticated LAN peers with
per-member fallback for anyone not live-complete.

**Phase 3 (study, not commitment):** a constrained BLE live path for 1–2 very
close peers, only if Phase 1/2 prove the demand and the reassembler/flow-control
work is worth it. Likely never.

## Sequencing against the refactor

Build **after the D-wave** settles: live PTT is a genuinely new lane that
touches transport + a new capture pipeline + libopus, and it must sit on top of
the consolidated mesh/session, not race it. It shares no policy with the
Wave-C/D consolidation. It is also gated on the honest question below.

## The honest recommendation

Live PTT is real and buildable, but v1 is a **narrow slice** — 1:1,
half-duplex, both users foreground on the *same friendly LAN*. On a cruise ship
where APs isolate clients, the LAN path often won't exist, so PTT will
frequently fall back to voice messages; it shines on a home/cabin network. Given
that, plus the libopus dual-encode cost, the go/no-go is: **is walkie-talkie-
on-friendly-networks worth a new native codec dependency and a new capture
pipeline, when async voice messages already cover the offline/ship case?** My
lean: worth prototyping Phase 1 *after* the media two-plane and the D-wave,
because voice messages (once the tail-clip fix lands) already deliver most of
the "send my voice" value with none of the real-time fragility.

## Appendix: where the three design passes converged / diverged

- **Converged, independently and with high confidence:** LAN-only for v1;
  never relay; async fallback is mandatory; never pause the mesh;
  authenticated contacts only.
- One pass contributed the quantitative breakpoints the others did not
  compute: BLE fan-out N, the latency budget, and the arithmetic that rules
  relay out.
- One contributed the code-cited blockers — envelope overhead, the strict
  reassembler, gossip latency — that rule out reusing the message pipeline.
  These are the "why not just…" answers.
- One contributed the architecture: dual-encode, the 0x07 wire protocol,
  signed START, COMPLETE-with-digest, the state machines, and the jitter
  policy. It also caught the load-bearing correction that the fallback must
  be AAC, rather than the invalid "reframe Opus" approach carried over from
  the earlier voice-messages draft.
- No genuine contradictions surfaced between the three. The divergence was in
  scope of detail, not in direction.
