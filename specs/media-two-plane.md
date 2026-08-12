# Media over two planes: design specification

Status: Proposed (rev 1)
Platforms: Android and iOS, with relayd additions
Scope: photos at full quality, video clips, and the boundary that keeps both
out of the delay-tolerant message pipeline

## Outcome

A person can send a photo or a video clip in CruiseMesh and have it behave
the way the rest of the app behaves: the message never goes missing, the
bytes arrive as fast as the current network genuinely allows, and nothing
about it silently spends someone else's battery, storage, or money.

Concretely, after this design ships:

- Every recipient sees the message — sender, caption, thumbnail, size —
  immediately and unconditionally, over any path CruiseMesh has, including
  Bluetooth and carried delivery. The conversation is never missing an entry.
- Full-resolution bytes arrive opportunistically: within seconds when both
  phones share a ship or home network, when the recipient chooses to use the
  internet, or automatically later when a capable path appears.
- Photos stop being recompressed to fit the messaging pipeline. Video clips
  become possible at all.

## The one architectural rule

CruiseMesh gains a second data plane, and the two planes never mix:

| | Message plane (exists today) | Blob plane (new) |
|---|---|---|
| Carries | text, receipts, service kinds, attachment **manifests and thumbnails** | full-resolution photo and video **bytes** |
| Size per item | bounded by the sealed-envelope pipeline as today | bounded only by blob-plane policy |
| Transports | all of them: BLE, LAN, relay, carried hop-by-hop | bulk TCP paths only: LAN, relay blob store |
| Delivery model | push, store-and-forward, delay-tolerant | **pull**, by the recipient, resumable |
| Third-party phones | may carry and re-offer envelopes | **never** touch blob bytes |
| Failure behavior | waits indefinitely; that is the product | waits indefinitely too — but as a pending download, visible to its owner |

The message plane is universal and therefore expensive per byte: everything
in it is eligible to sit in another family member's carry queue, re-offer
against per-encounter spray budgets, occupy the family's shared relay
mailbox, and cross Bluetooth at single-digit KB/s. The blob plane is cheap
per byte and therefore not universal. Every design decision below follows
from refusing to blur that line.

## Problem

Attachments today are capped at a small blob size and ride the ordinary
envelope pipeline. The cap is not an aesthetic choice: a delay-tolerant mesh
spends *other people's* resources on every authored byte — courier storage on
family phones, per-encounter Bluetooth airtime, the family's shared relay
quota. The cap is what keeps a photo from monopolizing all three. The cost
is real, though: photos are visibly recompressed, and video is impossible.

Raising the cap would be the wrong fix. A single 50 MB clip in the universal
pipeline is over an hour of monopolized Bluetooth at realistic GATT
throughput, a meaningful fraction of the family's relay storage, and a
standing occupant of every courier's queue — the exact failure classes the
mesh's budget work exists to prevent.

## Non-goals

- **No real-time calls.** Store-and-forward cannot carry them and the
  product is not trying to (the voice recommendation is push-to-talk bursts
  on the existing attachment pipeline; that is a separate, smaller effort on
  shipped infrastructure).
- **No raise of the universal-pipeline attachment cap.** Small attachments
  and voice bursts continue to ride it unchanged; that is a feature, not a
  limitation — they work everywhere, including over Bluetooth and carry.
- **No blob bytes over BLE**, in any mode, including "just this once."
- **No third-party carry of blob bytes** in v1. A courier phone never
  stores, forwards, or serves another person's blob. (A consented
  mule-assist mode is listed under Future directions; it is explicitly not
  in scope now.)
- **No new wire protocol for the message plane.** Manifests use the already
  allocated attachment kinds; the message plane's frames do not change.
- **No automatic spending.** Blob transfer over a metered, roaming, or
  expensive path never starts without an explicit, size-aware user action —
  composing with the roaming-deferral policy rather than duplicating it.

## Message plane: the manifest is the message

Sending a photo or clip authors one ordinary attachment message containing:

- media type, byte size, dimensions/duration;
- the **content digest of the encrypted blob** (the blob's permanent name);
- the **blob key** (see Security), sealed to recipients like any message
  content;
- a thumbnail (photos: long edge bounded, visually useful; video: poster
  frame), sized so manifest + thumbnail together fit comfortably inside
  today's attachment envelope bound.

This message is delay-tolerant like any other: it carries, it mules, it
relays, it survives partitions, receipts cover it. Whatever happens to the
bytes, the *conversation* is complete on every device.

Rules:

- The thumbnail is mandatory, generated at send time, and is the only
  degradation the message plane ever performs. There is no "full quality
  over the message plane" escape hatch.
- The manifest's digest names the encrypted bytes, so any copy fetched from
  anywhere can be verified before it is shown or stored.
- Deleting the local original after sending does not invalidate the
  manifest; it only limits which sources can still serve the blob.

## Blob plane: recipients pull, transfers resume

### Sources

A blob can be fetched from, in preference order:

1. **The sender's phone over LAN** — when both phones hold a live LAN link
   (the mesh already maintains authenticated TCP links on ship and home
   networks), the recipient requests chunks over a bulk sub-channel of that
   link. This is the primary path and the reason video is viable at all: a
   ship or home AP moves tens of megabytes in seconds to a minute.
2. **The relay blob store** — the sender may upload the encrypted blob to a
   new relayd blob endpoint (subject to consent and quota, below); any
   recipient with the manifest may then fetch it over the internet at their
   own pace.

Both sources serve the *same* encrypted, content-addressed bytes; a transfer
may begin against one source and finish against another.

### Chunking and resume (the "did we reinvent TCP" answer: no)

Within one TCP connection, ordering and reliability come from TCP. The blob
plane adds only what TCP cannot give across connections, days, and sources:

- The blob is divided into fixed-size chunks (LAN chunk size tuned for the
  link; relay fetches may use larger ranges). Chunk boundaries are derived
  from the digest-named blob, so they are identical at every source.
- The recipient persists a per-blob **chunk bitmap**. A resumed transfer —
  after an app restart, a network change, or switching source — requests
  exactly the missing ranges.
- On completion the assembled ciphertext is verified against the manifest
  digest before decryption; a mismatch discards the blob and re-requests it,
  and is a contract violation worth an event record if a *authenticated*
  source served it.
- All resume/scheduling policy (which source, which ranges, when to retry,
  when to defer) is pure core policy with table-driven tests. Platform code
  moves bytes.

### Consent and cost rules

- **LAN transfers auto-start** by default (they are free and local), bounded
  by a concurrent-transfer and bandwidth-courtesy policy so a burst of clips
  does not saturate the link the mesh itself is using. Auto-download on LAN
  is a per-device setting; default on.
- **Relay downloads and uploads over an expensive path never auto-start.**
  The recipient sees size before deciding. This composes with the
  roaming-deferral verdict: a roaming network defers blob transfer exactly
  as it defers relay sync, with the same Advanced override.
- **Relay upload is a sender decision** with the size shown, framed in user
  terms ("make this available over the internet"). On an ordinary unmetered
  network it may be a default-on convenience; on expensive paths it follows
  the consent rule.

### Quotas, expiry, and cleanup

- The relay blob store has a **per-family byte quota, separate from the
  mailbox quota**, enforced by relayd; the sender's app shows remaining
  space in Advanced. Blobs expire aggressively (days, not weeks) — the relay
  is a transfer window, not an album. Expiry of the relay copy never touches
  the manifest or anyone's completed download.
- On-device, received blobs live in the media store like any received
  photo/video (the user's own space); partially fetched chunk sets have a
  bounded budget and are garbage-collected oldest-first when it is exceeded.
- relayd enforces per-request range limits and the same family rate-limit
  discipline as the mailbox endpoints.

## Security and privacy

- **Blobs are encrypted before they are named.** The sender encrypts with a
  fresh per-blob key; the digest names the ciphertext; the key travels only
  inside the sealed manifest. The relay and any future assisting party store
  and serve bytes they cannot read — the same posture as sealed envelopes.
- One blob, many recipients: a group send seals the same blob key to each
  recipient's manifest copy; the ciphertext uploads once and is fetched per
  recipient.
- **The endpoint-privacy invariant is untouched.** A recipient fetches over
  LAN only from a link the mesh already authenticated with the sender, and
  otherwise from the relay. Nothing about the blob plane discovers, stores,
  or forwards any third party's address.
- Blob fetch requests carry no more identity than the mailbox protocol
  already does; possession of a manifest (digest + sealed key) is the
  capability to fetch and read.

## Proposed contract invariants

To be added to the protocol contract with executable owners when this ships:

- **BLOB-01 — plane separation.** Blob bytes never enter an envelope, a
  carry queue, a digest spray plan, or any BLE frame. (The existing spray
  and carry tests gain blob-flavored adversarial cases.)
- **BLOB-02 — ciphertext addressing.** The wire and the relay only ever see
  encrypted bytes; the blob key exists only inside sealed message content;
  digests name ciphertext.
- **BLOB-03 — pull with consent.** No blob transfer starts on an expensive
  or roaming path without an explicit user action; no third party is ever
  asked to move blob bytes.
- **BLOB-04 — bounded everywhere.** Device partial-chunk budget, relay
  family quota, relay expiry, and per-request range limits are all enforced
  and tested; a blob transfer terminates or defers inside declared budgets
  like every other pass.
- **BLOB-05 — verify before trust.** No blob is decrypted, shown, or
  retained without matching its manifest digest.

## UX requirements (family-obvious surface)

- A media message always renders instantly as thumbnail + size; the bubble
  is never blank and never blocks the conversation.
- Pending state is calm and truthful, reusing the connection-details
  vocabulary: "Will download when you're near Dad" / "Waiting for internet" /
  "Tap to download over the internet (34 MB)". Never a warning color for
  ordinary waiting.
- Progress and pause/resume on the bubble for large transfers; a failed
  chunk set retries quietly, and a verify failure reads as a plain retryable
  error, not jargon.
- Defaults: auto-download on LAN and unmetered Wi-Fi; ask first on relay
  when the path is expensive; per-chat override in the chat's settings;
  everything else behind Advanced.
- Sender-side: video capture/pick offers a clip-length guideline rather
  than a hard tiny cap, with the real bound coming from blob-plane policy;
  the size is always shown before send when the relay upload consent
  applies.

## Sizing (initial numbers, tunable from field evidence)

| Parameter | Initial value | Rationale |
|---|---|---|
| Thumbnail budget | fits manifest + thumbnail in today's attachment envelope | keeps the message plane unchanged |
| Blob size cap (v1) | 128 MB | covers phone video clips; not a movie service |
| LAN chunk | 256 KiB | small enough to interleave with mesh traffic on the same link |
| Relay range cap | 4 MiB per request | plays fair with mailbox endpoints under the family limiter |
| Relay blob quota | 512 MiB per family, separate from mailbox | a transfer window, not storage |
| Relay blob expiry | 7 days | matches the delivery-window philosophy |
| Partial-chunk GC budget (device) | 512 MiB, oldest first | bounded incompleteness |

## Delivery phases

**Phase 1 — LAN-only, photos first.** Manifests + thumbnails on the message
plane; blob fetch over the existing authenticated LAN links with chunk
bitmap and resume; photos ship full-quality; video behind the same machinery
once soak-tested. No relayd changes. This alone transforms the on-ship and
at-home experience.

**Phase 2 — relay blob store.** relayd blob endpoints (upload, ranged fetch,
quota, expiry), sender consent flow, recipient size-aware download, roaming
composition. This makes media work apart, not just together.

**Phase 3 — polish and study.** Group-send efficiencies, per-chat policies
informed by field metrics, and a *study* (not a commitment) of consented
mule-assist for blobs — whether a plugged-in, opted-in family device may
courier encrypted blobs under its own storage grant.

## Sequencing

This is deliberately a new plane rather than a change to the existing one: no
policy here duplicates anything the in-flight mesh/session consolidation is
unifying, and none of it should land inside that work. Build it after the
peer-encounter half of that consolidation settles, since its budgets are the
ones BLOB-01 must respect. Then proceed as its own series: the core policy
module first (chunking, bitmap, source selection, consent verdicts — pure and
table-tested), then the LAN bulk sub-channel drivers, then relayd. Voice
push-to-talk does not wait for any of this; it rides the existing pipeline
today.

## Acceptance criteria (Phase 1)

- A 30-second clip sent between two phones on one Wi-Fi network completes
  in well under a minute and survives an app kill + relaunch mid-transfer,
  resuming from the bitmap rather than restarting.
- The same send with the phones apart shows thumbnail + pending state on
  the recipient immediately via BLE/carry/relay, and auto-completes on the
  next shared network without user action.
- A blob transfer in progress never increases message-plane latency
  measurably (the courtesy bound is tested, not hoped).
- Zero blob bytes observable in any envelope, spray plan, carry queue, or
  BLE frame under an adversarial test that sends media continuously.
- Digest-verification failure discards, retries, and never renders.
- All copy through resources; the localization gate passes; the pending
  states reuse the connection-details vocabulary.
