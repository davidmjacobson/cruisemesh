# CruiseMesh Security Design

*A standalone description of what CruiseMesh encrypts, what it leaks, what it
deliberately traded away, and what has and has not been reviewed. Written for
a skeptical outside reader; the authoritative protocol spec lives in
[DESIGN.md](DESIGN.md) §6–§7.*

## Audit status — read this first

**CruiseMesh has not had an independent security review.** The design aims to
be boring enough to verify by reading (one page of constructions, no novel
cryptography), but nobody unaffiliated has yet tried to break it. Treat every
claim below as "by design," not "as verified." A paid design review is a
precondition the project has set for itself before recommending CruiseMesh
beyond its stated threat model.

## Threat model

CruiseMesh's adversary is **"no internet," not a nation-state.**

It is a delay-tolerant family messenger: messages travel over Bluetooth LE
between phones, get physically carried ("muled") by other phones, and
optionally sync through a self-hostable relay server when any phone finds
internet. Because ciphertext rides on strangers' phones and third-party
servers, everything is end-to-end encrypted — but the design does **not**
attempt anonymity, censorship resistance, metadata-free operation, or
resistance to a global passive observer. If your threat model includes state
actors, use something built for that (e.g. Briar) and accept its platform
limits.

### In scope

- Relays and mules must learn nothing about message **contents**, **sender
  identity**, or **read state**.
- Strangers must not be able to forge messages or receipts from your
  contacts, or join your groups.
- A compromised relay must yield only sealed envelopes and routing hints.
- Key material must never leave the device.

### Out of scope (accepted limitations)

- **Traffic analysis:** a relay or nearby observer learns timing, envelope
  counts, approximate message sizes (bucketed, see below), and the
  approximate size of your social graph.
- **Availability attacks:** BLE jamming, flooding, or simply being out of
  range. The medium is cooperative and physical.
- **Compromised endpoint:** if your phone is compromised, your messages are
  compromised. No messenger fixes this.
- **Recipient-compromise forward secrecy** — deliberately traded away; see
  "Why no Double Ratchet" below.

## Identity

- An identity is an **Ed25519 signing keypair + X25519 encryption keypair**,
  generated on device; private keys never leave it (no multi-device, no
  cloud backup of keys in v1 — a lost phone means re-friending).
- **UserID** = first 16 bytes of BLAKE2b(Ed25519 public key), shown base32
  for out-of-band sharing.
- **Friending** exchanges names and both public keys via QR code or pasted
  string, normally in person. Contact cards display a 4-word fingerprint
  phrase for verbal verification (Signal-safety-number style). Trust is
  TOFU-with-verification: the QR/string you scanned *is* the key you talk
  to; verify the phrase aloud if you didn't control the channel.

## Message encryption (1:1)

Per-message **sign-then-seal**, using libsodium constructions whole:

1. The plaintext body (which includes sender ID, chat ID, a per-chat Lamport
   counter, timestamp, and payload) is **signed** with the sender's Ed25519
   key.
2. The signed body is **sealed** to the recipient's X25519 key with an
   ephemeral sender key (`crypto_box_seal`-style ECIES with embedded sender
   authentication).
3. Plaintext is **padded to the next 256-byte bucket** before sealing so
   observers can't distinguish "ok" from a paragraph.

Groups use a shared symmetric **XChaCha20-Poly1305 group key**, distributed
pairwise-sealed (as above) by the group creator; group messages are signed by
the individual sender then encrypted with the group key. Membership removal
rotates the key and re-invites; a removed member can read only pre-rotation
traffic. This is family-scale group crypto by intent — no MLS, no tree KEMs.

Receipts (delivered/read) are ordinary sealed envelopes, indistinguishable
from messages on the wire: **mules and relays learn nothing about read
state.** Receipts are cumulative ("through Lamport N"), idempotent, and
re-sent opportunistically, so a lost receipt heals itself.

## What an observer sees

The public envelope header contains only:

```
version | msg_id (random 16B) | hop_ttl | expiry | recipient_hint | ciphertext
```

`recipient_hint` is an 8-byte BLAKE2b of (recipient UserID ‖ UTC-day salt) —
enough for relays and mules to route and for recipients to cheaply test "for
me?", **rotating daily** so there is no stable global identifier on the wire.
Sender identity is inside the ciphertext. The relay server stores sealed
envelopes and hints, nothing else, with bounded retention.

What a compromised relay or malicious mule learns: traffic timing, volume,
bucketed sizes, daily-rotating hints, and rough social-graph scale. What it
does not learn: contents, senders, recipients' stable identities, read
state, or group membership.

## Why no Double Ratchet (the biggest deliberate trade-off)

Ratchets assume ordered-ish, online-ish delivery. Delay-tolerant networking
provides neither: messages arrive hours late, out of order, or twice, and
there is no side channel to heal a desynchronized ratchet on a ship with no
internet. A desynced ratchet means **silently lost messages — the one
failure mode this app exists to prevent.**

So v1 uses per-message ephemeral keys instead: this gives confidentiality
and sender-side forward secrecy, and gives up recipient-compromise forward
secrecy (someone who steals the recipient's long-term key and has recorded
past ciphertext can decrypt it). For "meet at the buffet at 6," robustness
won. The envelope carries a `version` byte precisely so a ratchet or
post-quantum upgrade can ship later without a flag day.

This is a considered position, not an oversight — but it is exactly the kind
of judgment an independent review should probe.

## Broadcast mode

The public "shout" channel is signed but encrypted to a fixed, well-known
key — i.e., **readable by anyone with the app, by design**. The UI labels it
as public. Do not put anything private in broadcast.

## Design principles (and their origin story)

The project's one hard crypto rule: **use crypto libraries whole; never
assemble primitives into bespoke constructions.** This rule exists because
of Bridgefy — a comparable BLE mesh messenger that was cryptanalytically
broken twice, including *after* adopting libsignal, by misusing correct
primitives ("Breaking Bridgefy, again," USENIX Security '22). CruiseMesh's
defense is staying simple enough that the whole construction fits on this
page.

## Reporting

Think you've found a hole? See [SECURITY.md](SECURITY.md) — private
reporting, please.
